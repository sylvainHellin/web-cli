// add-expense: parametrised Rydoo draft-expense creation, driven with Playwright
// locators directly (label / data-testid anchored, auto-waiting) rather than by
// chaining snapshot refs. Runs INSIDE the daemon, which owns the Page.
//
// Hard rule: this code NEVER clicks Submit. It clicks Save only, and asserts the
// button's accessible name is exactly "Save" before clicking. On dry-run it fills
// and verifies everything, then Cancels without saving. On any failure it tries
// to leave the form Cancelled rather than half-saved.

import { existsSync } from "node:fs";
import type { Page, Locator } from "playwright";

export interface AddExpenseSpec {
  amount: string; // decimal comma ok, e.g. "14" or "109,50"
  currency: "EUR" | "USD";
  merchant: string;
  date: string; // dd/mm/yyyy
  category: string;
  project: { filter: string; option: string }; // filter text + exact option label
  department: string[]; // acceptable options in preference order, e.g. ["N/A","Development RBG"]
  investor: string | null; // exact option label, or null = leave empty
  location: string;
  comment: string;
  invoice: string; // absolute path to receipt PDF
  eurOverride: string | null; // converted EUR amount (decimal comma) or null
  bankProof: string | null; // absolute path, or null
}

export interface AddExpenseResult {
  entry: { merchant: string; date: string; amount: string; currency: string };
  saved: boolean;
  verifiedEur: string | null; // value read back from [data-testid=converted-amount], or null
  attachments: number;
  warnings: string[];
}

const T = {
  short: 5000,
  med: 15000,
  long: 30000,
};

function isSpec(x: unknown): x is AddExpenseSpec {
  if (!x || typeof x !== "object") return false;
  const s = x as Record<string, unknown>;
  const need = ["amount", "currency", "merchant", "date", "category", "project", "department", "location", "comment", "invoice"];
  for (const k of need) if (!(k in s)) throw new Error(`spec missing field: ${k}`);
  if (typeof s.amount !== "string") throw new Error("spec.amount must be a string");
  if (s.currency !== "EUR" && s.currency !== "USD") throw new Error('spec.currency must be "EUR" or "USD"');
  if (!Array.isArray(s.department) || s.department.length === 0) throw new Error("spec.department must be a non-empty array");
  const p = s.project as Record<string, unknown> | null;
  if (!p || typeof p.filter !== "string" || typeof p.option !== "string") throw new Error("spec.project needs {filter, option}");
  return true;
}

// Small helpers -------------------------------------------------------------

async function settle(page: Page, ms = 400): Promise<void> {
  await page.waitForTimeout(ms);
}

// Click a cp-select field, filter its search, and pick a matching option from the
// CDK overlay. `acceptable` is a preference-ordered list; the first option that is
// actually present is chosen. Returns the chosen label. Throws if none match.
async function pickSelect(page: Page, field: Locator, filter: string, acceptable: string[]): Promise<string> {
  // Ensure no previous select overlay is still open (it would steal our clicks /
  // confuse pane resolution).
  await closeOverlay(page);
  const optionSel = "cp-select-option, [role=option]";
  const anyOption = page.locator("cp-select-option:visible, [role=option]:visible");
  // Open the overlay: click the field, then confirm options actually appeared.
  // Angular re-renders these cp-select nodes (a resolved locator can detach mid
  // call) and a single click sometimes lands without opening the CDK overlay, so
  // click + verify + retry with a freshly-resolved locator.
  let opened = false;
  for (let attempt = 0; attempt < 4 && !opened; attempt++) {
    try {
      await field.waitFor({ state: "visible", timeout: T.med });
      await field.click({ timeout: T.short });
    } catch (e) {
      const msg = (e as Error).message || "";
      if (!/not attached|detached|stale|Element is not|Timeout/i.test(msg)) throw e;
      await settle(page, 400);
      continue;
    }
    opened = await anyOption
      .first()
      .waitFor({ state: "visible", timeout: 2500 })
      .then(() => true)
      .catch(() => false);
    if (!opened) await settle(page, 400);
  }
  if (!opened) throw new Error("select overlay did not open after clicking the field");
  await settle(page, 200);
  // The option list lives in its OWN cdk-overlay-pane (never the form pane). Pin to
  // the pane that actually contains options; this avoids typing the filter into a
  // form input by mistake. Require a VISIBLE option before proceeding, which also
  // sidesteps stale detached panes CDK leaves behind.
  const optionPane = page
    .locator(".cdk-overlay-pane")
    .filter({ has: page.locator(`${optionSel.split(", ").map((s) => `${s}:visible`).join(", ")}`) })
    .last();
  await optionPane.locator(optionSel).first().waitFor({ state: "visible", timeout: T.med });
  if (filter) {
    // Type the filter into the option pane's OWN search box, if it has one.
    const search = optionPane.locator("input").first();
    if (await search.count()) {
      try {
        await search.fill(filter, { timeout: T.short });
        await settle(page, 1000);
      } catch {
        /* no usable search box; scan the full list */
      }
    }
  }
  const options = optionPane.locator(optionSel);
  await options.first().waitFor({ state: "visible", timeout: T.med });
  // Match modes, tried per preference-ordered acceptable value:
  //  1. exact trimmed text  2. starts-with (currency "USD" -> "USDUS dollar")
  //  3. contains. First hit wins.
  const matchers = [
    (w: string) => options.filter({ hasText: new RegExp(`^\\s*${escapeRe(w)}\\s*$`) }),
    (w: string) => options.filter({ hasText: new RegExp(`^\\s*${escapeRe(w)}`) }),
    (w: string) => options.filter({ hasText: w }),
  ];
  for (const make of matchers) {
    for (const want of acceptable) {
      const cand = make(want);
      if (await cand.count()) {
        await cand.first().click({ timeout: T.med });
        await settle(page, 400);
        return want;
      }
    }
  }
  const seen = (await options.allInnerTexts()).map((t) => t.trim()).filter(Boolean).slice(0, 30);
  throw new Error(`none of [${acceptable.join(", ")}] found in options: [${seen.join(" | ")}]`);
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Close any open CDK overlay (e.g. a select) by pressing Escape.
async function closeOverlay(page: Page): Promise<void> {
  try {
    if (await page.locator(".cdk-overlay-pane cp-select-option").first().isVisible({ timeout: 500 })) {
      await page.keyboard.press("Escape");
      await settle(page, 200);
    }
  } catch {
    /* ignore */
  }
}

// Field locators, all anchored on stable data-testid / label text --------------

function amountInput(page: Page): Locator {
  return page.locator('cp-input[data-testid="amount"] input').first();
}
function merchantInput(page: Page): Locator {
  return page.locator('cp-input[data-testid="merchant"] input').first();
}
function dateInput(page: Page): Locator {
  // visible flatpickr alt input (type=text), not the hidden ISO one
  return page.locator('cp-input[data-testid="expense-date"] input[type="text"]').first();
}
function currencySelect(page: Page): Locator {
  return page.locator('cp-select[data-testid="currency"] .cp-select__field').first();
}
function categorySelect(page: Page): Locator {
  return page.locator('cp-select[data-testid="category"] .cp-select__field').first();
}
function projectSelect(page: Page): Locator {
  return page.locator('cp-select[data-testid="project"] .cp-select__field').first();
}
function saveButton(page: Page): Locator {
  return page.locator('cp-button[data-testid="save-button"] button').first();
}
function cancelButton(page: Page): Locator {
  return page.locator("button", { hasText: /^\s*Cancel\s*$/ }).first();
}

// Resolve a custom field by scanning for the nearest label (JS-side), returning a
// Playwright Locator pinned to that specific cp-select via a generated marker.
async function customFieldByLabel(page: Page, label: string): Promise<Locator> {
  const marker = `pwmark-${label.toLowerCase().replace(/[^a-z]/g, "")}`;
  const found = await page.evaluate(
    ({ label, marker }) => {
      const selects = Array.from(document.querySelectorAll('cp-select[data-testid="custom-field-list"]'));
      for (const s of selects) {
        let cur: Element | null = s;
        for (let i = 0; i < 8 && cur; i++) {
          cur = cur.parentElement;
          if (cur) {
            const l = cur.querySelector("label");
            if (l && l.textContent && l.textContent.trim() === label) {
              (s as HTMLElement).setAttribute("data-pwmark", marker);
              return true;
            }
          }
        }
      }
      return false;
    },
    { label, marker },
  );
  if (!found) throw new Error(`custom field "${label}" not found`);
  return page.locator(`cp-select[data-pwmark="${marker}"] .cp-select__field`).first();
}

// Set the contenteditable comment via insertText (matches the manual recipe).
async function setComment(page: Page, text: string): Promise<void> {
  const ok = await page.evaluate((t) => {
    const host = document.querySelector('[data-testid="comment"]');
    const ce = host?.querySelector('[contenteditable="true"], [contenteditable=""], .cp-input__textarea--editable') as HTMLElement | null;
    if (!ce) return false;
    ce.focus();
    // clear then insert
    const sel = window.getSelection();
    const range = document.createRange();
    range.selectNodeContents(ce);
    sel?.removeAllRanges();
    sel?.addRange(range);
    document.execCommand("insertText", false, t);
    ce.dispatchEvent(new Event("input", { bubbles: true }));
    return (ce.textContent || "").includes(t);
  }, text);
  if (!ok) throw new Error("failed to set comment (contenteditable not found or text not applied)");
}

// Open the Add Expense form from the expenses list.
async function openForm(page: Page): Promise<void> {
  await page.locator('button[data-testid], button', { hasText: /^\s*Add Expense\s*$/ }).first().click({ timeout: T.med }).catch(async () => {
    // fall back to the cp-button host
    await page.locator("cp-button", { hasText: /Add Expense/ }).first().click({ timeout: T.med });
  });
  await settle(page, 500);
  await page.locator("cp-dropdown-item", { hasText: /^\s*Add Expense\s*$/ }).first().click({ timeout: T.med });
  await page.waitForURL(/new-regular-expense/, { timeout: T.long });
  await amountInput(page).waitFor({ state: "visible", timeout: T.long });
  await settle(page, 600);
}

// Verify all filled values took (used in dry-run and before save).
async function verifyValues(page: Page, spec: AddExpenseSpec, warnings: string[]): Promise<void> {
  const amount = (await amountInput(page).inputValue()).trim();
  const wantAmount = spec.amount.replace(",", ".");
  const gotAmount = amount.replace(",", ".");
  if (!gotAmount || parseFloat(gotAmount) !== parseFloat(wantAmount)) {
    throw new Error(`amount mismatch: form has "${amount}", spec wants "${spec.amount}"`);
  }
  const merchant = (await merchantInput(page).inputValue()).trim();
  if (merchant !== spec.merchant) throw new Error(`merchant mismatch: form "${merchant}" vs spec "${spec.merchant}"`);
  const dateVal = (await dateInput(page).inputValue()).replace(/\s/g, "");
  const wantDate = spec.date.replace(/\s/g, "").replace(/\//g, "/");
  if (dateVal.replace(/\//g, "") !== wantDate.replace(/\//g, "")) {
    throw new Error(`date mismatch: form "${dateVal}" vs spec "${spec.date}"`);
  }
  // Selects: the chosen value lives in the cp-select's inner <input> (the host
  // element's textContent reads back empty; verified live).
  const selectVal = async (testid: string) =>
    (await page.locator(`cp-select[data-testid="${testid}"] input`).first().inputValue()).trim();
  const curText = await selectVal("currency");
  if (!curText.includes(spec.currency)) warnings.push(`currency field shows "${curText}", expected ${spec.currency}`);
  const catText = await selectVal("category");
  if (!catText.includes(spec.category)) warnings.push(`category field shows "${catText}", expected "${spec.category}"`);
  const projText = await selectVal("project");
  if (!projText.includes(spec.project.option)) warnings.push(`project field shows "${projText}", expected "${spec.project.option}"`);
  // Custom fields (Department / Investor / Location): same inner-input readback,
  // resolved by walking up from each custom-field select to its label.
  const custom = (await page.evaluate(() => {
    const out: Record<string, string> = {};
    for (const s of Array.from(document.querySelectorAll('cp-select[data-testid="custom-field-list"]'))) {
      let cur: Element | null = s;
      for (let i = 0; i < 8 && cur; i++) {
        cur = cur.parentElement;
        const l = cur?.querySelector("label");
        if (l && l.textContent) {
          const inp = s.querySelector("input") as HTMLInputElement | null;
          out[l.textContent.trim()] = inp ? inp.value.trim() : "";
          break;
        }
      }
    }
    return out;
  })) as Record<string, string>;
  if (!spec.department.includes(custom["Department"] ?? "")) {
    warnings.push(`department shows "${custom["Department"] ?? ""}", expected one of [${spec.department.join(", ")}]`);
  }
  if (spec.investor && (custom["Investor"] ?? "") !== spec.investor) {
    warnings.push(`investor shows "${custom["Investor"] ?? ""}", expected "${spec.investor}"`);
  }
  if ((custom["Location"] ?? "") !== spec.location) {
    warnings.push(`location shows "${custom["Location"] ?? ""}", expected "${spec.location}"`);
  }
  const comment = await page.evaluate(() => {
    const host = document.querySelector('[data-testid="comment"]');
    const ce = host?.querySelector('[contenteditable], .cp-input__textarea--editable');
    return (ce?.textContent || "").trim();
  });
  if (comment !== spec.comment) warnings.push(`comment shows "${comment}", expected "${spec.comment}"`);
}

// Fill the whole form (does not save). Returns chosen department label.
async function fillForm(
  page: Page,
  spec: AddExpenseSpec,
  warnings: string[],
  mark: (s: string) => void = () => {},
): Promise<{ chosenDept: string }> {
  // Amount
  mark("amount");
  await amountInput(page).fill(spec.amount, { timeout: T.med });
  // Currency (only touch for non-EUR; EUR is the default). Always set to be safe.
  if (spec.currency !== "EUR") {
    mark("currency");
    await pickSelect(page, currencySelect(page), spec.currency, [spec.currency]);
  }
  // Merchant
  mark("merchant");
  await merchantInput(page).fill(spec.merchant, { timeout: T.med });
  // Expense date: fill visible input then Enter to commit flatpickr
  const di = dateInput(page);
  await di.click({ timeout: T.med });
  await di.fill(spec.date, { timeout: T.med });
  await di.press("Enter");
  await settle(page, 400);
  // NOTE: do NOT press Escape here. With no select overlay open, Escape closes the
  // topmost CDK overlay, which is the sidepanel form itself (verified live: this
  // silently killed the form and every later step failed at "category").
  // Category
  mark("category");
  await pickSelect(page, categorySelect(page), spec.category, [spec.category]);
  // Project
  mark("project");
  await pickSelect(page, projectSelect(page), spec.project.filter, [spec.project.option]);
  // Department (preference-ordered acceptable options)
  mark("department");
  const deptField = await customFieldByLabel(page, "Department");
  const chosenDept = await pickSelect(page, deptField, "", spec.department);
  if (chosenDept !== spec.department[0]) warnings.push(`department fell back to "${chosenDept}" (first choice "${spec.department[0]}" unavailable)`);
  // Investor (optional)
  if (spec.investor) {
    mark("investor");
    const invField = await customFieldByLabel(page, "Investor");
    await pickSelect(page, invField, spec.investor, [spec.investor]);
  }
  // Location
  mark("location");
  const locField = await customFieldByLabel(page, "Location");
  await pickSelect(page, locField, spec.location, [spec.location]);
  // Comment
  mark("comment");
  await setComment(page, spec.comment);
  // Invoice upload (hidden file input)
  mark("invoice");
  if (!existsSync(spec.invoice)) throw new Error(`invoice not found: ${spec.invoice}`);
  await page.locator('input[type="file"]').first().setInputFiles(spec.invoice, { timeout: T.med });
  await settle(page, 1500);
  return { chosenDept };
}

// Apply the converted-amount (EUR) override on the reopened row, attach bank proof,
// save, and read back the override value. Returns the verified value or null.
async function applyEurOverride(page: Page, spec: AddExpenseSpec, warnings: string[]): Promise<{ verifiedEur: string | null; attachments: number }> {
  // Reopen the newly created row (match by merchant + date on the list)
  await page.waitForURL(/expenses\/personal(\/)?$/, { timeout: T.long }).catch(() => {});
  await settle(page, 800);
  const row = page.locator("cp-table-row, [role=row], tr").filter({ hasText: spec.merchant }).filter({ hasText: spec.date }).first();
  await row.waitFor({ state: "visible", timeout: T.med });
  await row.click({ timeout: T.med });
  await settle(page, 1200);
  // Find the pencil/edit button next to Converted amount
  const pencil = page.locator('[data-testid="converted-amount"]').locator("xpath=following::button[1]").first();
  let clicked = false;
  if (await pencil.count()) {
    await pencil.click({ timeout: T.med }).then(() => (clicked = true)).catch(() => {});
  }
  if (!clicked) {
    // fallback: any edit button inside the converted-amount block
    const alt = page.locator('cp-button', { has: page.locator('[data-icon], svg') }).filter({ hasText: "" });
    await alt.first().click({ timeout: T.short }).catch(() => {});
  }
  // Dialog "Converted amount (EUR)"
  const dialog = page.locator(".cdk-overlay-pane, [role=dialog]").filter({ hasText: /Converted amount/i }).first();
  await dialog.waitFor({ state: "visible", timeout: T.med });
  const dInput = dialog.locator("input").first();
  await dInput.fill(spec.eurOverride as string, { timeout: T.med });
  await dialog.locator("button", { hasText: /^\s*Save\s*$/ }).first().click({ timeout: T.med });
  await settle(page, 800);
  // Attach bank proof via "Add attachment"
  let attachments = 1;
  if (spec.bankProof) {
    if (!existsSync(spec.bankProof)) throw new Error(`bank proof not found: ${spec.bankProof}`);
    // The Add attachment control exposes a hidden file input; the newest input accepts it.
    const inputs = page.locator('input[type="file"]');
    await inputs.last().setInputFiles(spec.bankProof, { timeout: T.med });
    await settle(page, 1500);
    attachments = 2;
  }
  // Main Save (assert accessible name is exactly "Save")
  await assertSaveAndClick(page);
  await settle(page, 1500);
  // Verify override via [data-testid=converted-amount] on reopen
  await page.waitForURL(/expenses\/personal(\/)?$/, { timeout: T.long }).catch(() => {});
  await settle(page, 800);
  const row2 = page.locator("cp-table-row, [role=row], tr").filter({ hasText: spec.merchant }).filter({ hasText: spec.date }).first();
  await row2.click({ timeout: T.med });
  await settle(page, 1200);
  let verifiedEur: string | null = null;
  const conv = page.locator('[data-testid="converted-amount"]').first();
  if (await conv.count()) {
    verifiedEur = (await conv.innerText()).trim();
  } else {
    warnings.push("could not read [data-testid=converted-amount] on reopen");
  }
  // Close the panel without saving again
  await cancelButton(page).click({ timeout: T.short }).catch(async () => {
    await page.keyboard.press("Escape").catch(() => {});
  });
  await settle(page, 600);
  return { verifiedEur, attachments };
}

// Assert the primary save button reads exactly "Save" (never "Submit"), then click.
async function assertSaveAndClick(page: Page): Promise<void> {
  const btn = saveButton(page);
  await btn.waitFor({ state: "visible", timeout: T.med });
  const name = (await btn.innerText()).trim();
  if (name !== "Save") throw new Error(`refusing to click save: button reads "${name}", expected exactly "Save"`);
  await btn.click({ timeout: T.med });
}

// Verify "Expense saved" toast / confirmation appeared.
async function verifySavedToast(page: Page, warnings: string[]): Promise<void> {
  try {
    await page.locator("text=/Expense saved/i").first().waitFor({ state: "visible", timeout: T.short });
  } catch {
    warnings.push('did not observe "Expense saved" confirmation toast (may have auto-dismissed)');
  }
}

// Public entry point ---------------------------------------------------------

export async function addExpense(page: Page, rawSpec: unknown, dryRun: boolean): Promise<AddExpenseResult> {
  isSpec(rawSpec);
  const spec = rawSpec as AddExpenseSpec;
  const warnings: string[] = [];
  const result: AddExpenseResult = {
    entry: { merchant: spec.merchant, date: spec.date, amount: spec.amount, currency: spec.currency },
    saved: false,
    verifiedEur: null,
    attachments: 0,
    warnings,
  };

  let stage = "start";
  const mark = (s: string) => {
    stage = s;
  };
  try {
    mark("openForm");
    await openForm(page);
    mark("fillForm");
    const { chosenDept } = await fillForm(page, spec, warnings, mark);
    void chosenDept;
    mark("verifyValues");
    await verifyValues(page, spec, warnings);

    if (dryRun) {
      await closeOverlay(page);
      await cancelButton(page).click({ timeout: T.med });
      await page.waitForURL(/expenses\/personal(\/)?$/, { timeout: T.long }).catch(() => {});
      result.saved = false;
      result.attachments = 1; // invoice was attached in-form but discarded on cancel
      warnings.push("dry-run: form cancelled, nothing saved");
      return result;
    }

    // Real save
    await closeOverlay(page);
    await assertSaveAndClick(page);
    await verifySavedToast(page, warnings);
    result.saved = true;
    result.attachments = 1;

    if (spec.eurOverride) {
      const { verifiedEur, attachments } = await applyEurOverride(page, spec, warnings);
      result.verifiedEur = verifiedEur;
      result.attachments = attachments;
    }
    return result;
  } catch (err) {
    // Best-effort: leave the form cancelled rather than half-saved.
    try {
      await closeOverlay(page);
      if (await cancelButton(page).isVisible({ timeout: 1000 })) {
        await cancelButton(page).click({ timeout: T.short });
      }
    } catch {
      /* ignore */
    }
    const raw = (err as Error).message || String(err);
    throw new Error(`[stage:${stage}] ${raw.split("\n")[0].trim()}`);
  }
}
