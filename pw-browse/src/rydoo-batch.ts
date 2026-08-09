#!/usr/bin/env node
// rydoo-batch: deterministic monthly Rydoo draft-expense creation.
//
// "Script drives, agent repairs." Zero LLM calls at runtime. This is a
// standalone entry point that owns its OWN Playwright browser: it launches
// chromium.launchPersistentContext on the Rydoo profile, drives the form with
// pinned selectors + explicit waits + readback assertions, and closes the
// context on exit. It does NOT talk to the pw-browse daemon.
//
// Usage:
//   node dist/rydoo-batch.js --spec entries.json [--dry-run] [--entry <n>]
//
// Hard rules baked in:
//   - NEVER clicks a button whose accessible name is "Submit". The primary save
//     button's name is asserted to be exactly "Save" before every click.
//   - --dry-run fills + uploads + asserts readbacks, then Cancels and asserts the
//     list row count is unchanged. Nothing is saved.
//   - Any assertion miss stops the whole run with a precise message naming the
//     entry + step. No entry ever continues past a failed assertion, and a form
//     is never left open (best-effort Cancel on the way out).
//   - Precondition: we must reach the authenticated expenses list. A fresh
//     browser start always lands on the accounts.rydoo.com login page (Rydoo's
//     own session cookie is not persistent); there we perform a silent SSO
//     bounce: fill the email (--email, or the RYDOO_EMAIL env var), click
//     Next, and let Entra complete SSO without interaction. If that instead
//     reaches an interactive Microsoft page (password / MFA / Authenticator) or
//     stalls, we print a clear handoff message and exit 2. We NEVER type a
//     password and NEVER interact with MFA.

import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { resolve, isAbsolute, join } from "node:path";
import { chromium, type BrowserContext, type Page, type Locator } from "playwright";

// --------------------------------------------------------------------------
// Spec
// --------------------------------------------------------------------------

export interface Entry {
  amount: string; // decimal comma ok, e.g. "14" or "109,50"
  currency: "EUR" | "USD";
  merchant: string;
  date: string; // dd/mm/yyyy
  category: string;
  projectFilter: string; // text typed to filter the project dropdown
  projectLabel: string; // exact option label to pick and assert
  departmentPreference: string[]; // acceptable options, preference order, e.g. ["N/A","Development RBG"]
  investor: string | null; // exact option label, or null = leave empty
  location: string;
  comment: string;
  invoicePath: string; // absolute (or resolvable) path to receipt PDF
  eurOverride: string | null; // converted EUR amount (decimal comma) or null
  bankProofPath: string | null; // absolute (or resolvable) path, or null
}

export interface EntryResult {
  merchant: string;
  date: string;
  amount: string;
  currency: string;
  saved: boolean;
  verifiedEur: string | null; // value read back from [data-testid=converted-amount]
  attachments: number;
  warnings: string[];
}

const T = { short: 5000, med: 15000, long: 30000 };

const RYDOO_LIST_URL = "https://app.rydoo.com/expenses/personal";
const DEFAULT_PROFILE = join(homedir(), ".cache", "pw-browse-rydoo");
// SSO email for the silent login bounce. No hardcoded default: it comes from
// --email or the RYDOO_EMAIL env var, and the run stops if neither is set.
const ENV_EMAIL = process.env.RYDOO_EMAIL ?? "";

// A run-stopping, precisely-labelled assertion failure.
class BatchError extends Error {
  constructor(
    public entryNo: number,
    public merchant: string,
    public step: string,
    detail: string,
  ) {
    super(`entry ${entryNo} (${merchant}) @ ${step}: ${detail}`);
    this.name = "BatchError";
  }
}

// --------------------------------------------------------------------------
// Spec validation
// --------------------------------------------------------------------------

function validateEntry(e: unknown, i: number): Entry {
  if (!e || typeof e !== "object") throw new Error(`entry[${i}] is not an object`);
  const s = e as Record<string, unknown>;
  const str = (k: string): string => {
    if (typeof s[k] !== "string" || (s[k] as string).length === 0) throw new Error(`entry[${i}].${k} must be a non-empty string`);
    return s[k] as string;
  };
  const optStr = (k: string): string | null => {
    if (s[k] === null || s[k] === undefined) return null;
    if (typeof s[k] !== "string") throw new Error(`entry[${i}].${k} must be a string or null`);
    return s[k] as string;
  };
  const currency = str("currency");
  if (currency !== "EUR" && currency !== "USD") throw new Error(`entry[${i}].currency must be "EUR" or "USD"`);
  if (!Array.isArray(s.departmentPreference) || s.departmentPreference.length === 0 || !s.departmentPreference.every((d) => typeof d === "string")) {
    throw new Error(`entry[${i}].departmentPreference must be a non-empty string array`);
  }
  const eurOverride = optStr("eurOverride");
  const bankProofPath = optStr("bankProofPath");
  if (currency === "USD" && !eurOverride) {
    // Not fatal on its own (some USD entries may keep the default rate), but the
    // recurring set always overrides; warn loudly via a thrown error would be too
    // strict, so we allow it. Nothing to do here.
  }
  return {
    amount: str("amount"),
    currency,
    merchant: str("merchant"),
    date: str("date"),
    category: str("category"),
    projectFilter: str("projectFilter"),
    projectLabel: str("projectLabel"),
    departmentPreference: s.departmentPreference as string[],
    investor: optStr("investor"),
    location: str("location"),
    comment: str("comment"),
    invoicePath: str("invoicePath"),
    eurOverride,
    bankProofPath,
  };
}

function loadSpec(specPath: string): Entry[] {
  if (!existsSync(specPath)) throw new Error(`spec file not found: ${specPath}`);
  let raw: unknown;
  try {
    raw = JSON.parse(readFileSync(specPath, "utf8"));
  } catch (e) {
    throw new Error(`bad spec json: ${(e as Error).message}`);
  }
  if (!Array.isArray(raw)) throw new Error("spec must be a JSON array of entries");
  return raw.map((e, i) => validateEntry(e, i));
}

// Resolve a receipt path: expand a leading ~ and make it absolute.
function resolvePath(p: string): string {
  let out = p;
  if (out.startsWith("~/")) out = join(homedir(), out.slice(2));
  return isAbsolute(out) ? out : resolve(out);
}

// --------------------------------------------------------------------------
// Small helpers
// --------------------------------------------------------------------------

async function settle(page: Page, ms = 400): Promise<void> {
  await page.waitForTimeout(ms);
}

// Minimize the headed browser window so a launch never steals macOS focus.
// launchPersistentContext always brings the Chromium window to the front and
// captures keyboard focus; on a repeat monthly run that pops in front of
// whatever the user is typing into. We drive the OS-level window state over CDP
// (Browser.getWindowForTarget -> Browser.setWindowBounds windowState:minimized)
// rather than a viewport trick, because only the real window state releases
// focus. The anti-throttle Chromium flags below keep a minimized window
// responsive, so the form still drives normally while hidden. Best-effort: a
// failure here is logged and never aborts the run (the flags keep it working
// even if it stays visible).
async function minimizeWindow(context: BrowserContext, page: Page): Promise<boolean> {
  try {
    const cdp = await context.newCDPSession(page);
    const { windowId } = (await cdp.send("Browser.getWindowForTarget")) as { windowId: number };
    // Two-step: some Chromium builds reject setting minimized together with a
    // bounds rect, so send windowState alone.
    await cdp.send("Browser.setWindowBounds", { windowId, bounds: { windowState: "minimized" } });
    await cdp.detach().catch(() => {});
    return true;
  } catch (e) {
    process.stdout.write(`warning: could not minimize window (${(e as Error).message.split("\n")[0]}); it may steal focus\n`);
    return false;
  }
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Close any open CDK select overlay by pressing Escape. Never called when the
// only overlay is the form sidepanel itself (Escape would close the form).
async function closeSelectOverlay(page: Page): Promise<void> {
  try {
    if (await page.locator(".cdk-overlay-pane cp-select-option").first().isVisible({ timeout: 500 })) {
      await page.keyboard.press("Escape");
      await settle(page, 200);
    }
  } catch {
    /* ignore */
  }
}

// Wait until a select overlay's options have really loaded. Rydoo shows a
// transient placeholder ("Loading data", "Loading...", or a spinner) as the only
// option while it fetches the list; matching against that placeholder fails
// spuriously (observed on the category select on a cold-ish second launch).
// Poll until at least one option exists whose text is not a placeholder.
async function waitForOptionsLoaded(page: Page, options: Locator): Promise<void> {
  const placeholderRe = /^(loading data|loading\.{0,3}|loading|please wait|no results?|no data)$/i;
  const deadline = Date.now() + T.med;
  while (Date.now() < deadline) {
    const texts = (await options.allInnerTexts().catch(() => [])).map((t) => t.trim()).filter(Boolean);
    if (texts.length > 0 && texts.some((t) => !placeholderRe.test(t))) return;
    await settle(page, 250);
  }
  // Fall through: let the caller's matcher run and produce its precise error.
}

// Click a cp-select field, optionally filter, and pick a matching option from the
// CDK overlay. `acceptable` is preference-ordered; the first present option wins.
// Returns the chosen label. Throws if none match.
async function pickSelect(page: Page, field: Locator, filter: string, acceptable: string[]): Promise<string> {
  await closeSelectOverlay(page);
  const optionSel = "cp-select-option, [role=option]";
  const anyOption = page.locator("cp-select-option:visible, [role=option]:visible");
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
  const optionPane = page
    .locator(".cdk-overlay-pane")
    .filter({ has: page.locator(optionSel.split(", ").map((x) => `${x}:visible`).join(", ")) })
    .last();
  await optionPane.locator(optionSel).first().waitFor({ state: "visible", timeout: T.med });
  // The overlay can render a transient "Loading data" placeholder option while
  // Rydoo fetches the real list (categories, projects, custom fields are all
  // async). Scanning too early sees only that placeholder and fails. Wait until
  // a real, non-placeholder option is present before filtering/matching.
  await waitForOptionsLoaded(page, optionPane.locator(optionSel));
  if (filter) {
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
  await waitForOptionsLoaded(page, options);
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

// --------------------------------------------------------------------------
// Field locators, anchored on stable data-testid / label text
// --------------------------------------------------------------------------

const amountInput = (p: Page) => p.locator('cp-input[data-testid="amount"] input').first();
const merchantInput = (p: Page) => p.locator('cp-input[data-testid="merchant"] input').first();
// visible flatpickr alt input (type=text), not the hidden ISO one
const dateInput = (p: Page) => p.locator('cp-input[data-testid="expense-date"] input[type="text"]').first();
const currencySelect = (p: Page) => p.locator('cp-select[data-testid="currency"] .cp-select__field').first();
const categorySelect = (p: Page) => p.locator('cp-select[data-testid="category"] .cp-select__field').first();
const projectSelect = (p: Page) => p.locator('cp-select[data-testid="project"] .cp-select__field').first();
const saveButton = (p: Page) => p.locator('cp-button[data-testid="save-button"] button').first();
const cancelButton = (p: Page) => p.locator("button", { hasText: /^\s*Cancel\s*$/ }).first();

// Resolve a custom field (Department / Investor / Location) by scanning for its
// nearest label, pinning the specific cp-select via a generated marker attr.
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

// --------------------------------------------------------------------------
// List helpers
// --------------------------------------------------------------------------

async function listRowCount(page: Page): Promise<number> {
  await settle(page, 400);
  return page.locator("cp-table-row").count();
}

// Precondition: assert we are on the authenticated expenses list. If a login /
// MFA page is detected, throw a LoginRequired sentinel so main() can exit
// cleanly with the handoff message. Never attempts to log in.
class LoginRequired extends Error {}

// Silent SSO bounce. Rydoo's own session cookie does not survive a browser
// restart, so EVERY fresh start on the persistent profile lands on the
// accounts.rydoo.com login page even while the Microsoft Entra cookie is alive.
// Submitting just the email there hands off to Entra, which completes SSO
// silently (zero MFA) and lands back on the expenses list. This NEVER types a
// password and NEVER interacts with MFA: it fills the email, clicks the
// Next/Verify button, and waits. Returns true if we reach app.rydoo.com; false
// means interactive login is genuinely required (caller falls through to the
// LOGIN REQUIRED handoff).
async function attemptSilentSsoBounce(page: Page, email: string): Promise<boolean> {
  process.stdout.write(`Rydoo login page detected; attempting silent SSO bounce as ${email}\n`);
  try {
    const emailBox = page.getByRole("textbox", { name: /email/i }).or(page.locator('input[type="email"]')).first();
    await emailBox.waitFor({ state: "visible", timeout: T.med });
    await emailBox.fill(email, { timeout: T.short });
    await settle(page, 300);
    // The button reads "Verify email" before the email is filled and "Next" after.
    const nextBtn = page.getByRole("button", { name: /^(Next|Verify email)$/i }).first();
    await nextBtn.waitFor({ state: "visible", timeout: T.short });
    await nextBtn.click({ timeout: T.short });
    // Generous wait: the redirect chain transits login.microsoftonline.com
    // without stopping when the Entra cookie is persistent. If it instead parks
    // on an interactive Microsoft page (password / MFA / Authenticator), we
    // never reach app.rydoo.com and time out into the failure path below.
    await page.waitForURL(/app\.rydoo\.com/, { timeout: 45000 });
    await settle(page, 1000);
    if (!/expenses\/personal/.test(page.url())) {
      await page.goto(RYDOO_LIST_URL, { waitUntil: "domcontentloaded", timeout: T.long }).catch(() => {});
      await settle(page, 1000);
    }
    process.stdout.write(`silent SSO bounce succeeded (${page.url()})\n`);
    return true;
  } catch {
    process.stdout.write(`silent SSO bounce did not complete; stuck at ${page.url()}\n`);
    return false;
  }
}

async function assertOnAuthenticatedList(page: Page): Promise<void> {
  const url = page.url();
  if (/accounts\.rydoo\.com|login\.microsoftonline\.com|\/login|\/sign-in|\/authorize/i.test(url)) {
    throw new LoginRequired(`landed on an auth page: ${url}`);
  }
  // The list must be present. Wait for the Add Expense affordance OR a table.
  const listReady = await Promise.race([
    page
      .locator("cp-table, cp-table-row, [role=table]")
      .first()
      .waitFor({ state: "visible", timeout: T.long })
      .then(() => true)
      .catch(() => false),
    page
      .locator("button, cp-button", { hasText: /Add Expense/ })
      .first()
      .waitFor({ state: "visible", timeout: T.long })
      .then(() => true)
      .catch(() => false),
  ]);
  // Re-check the URL after the wait: an SSO bounce can complete late.
  const url2 = page.url();
  if (/accounts\.rydoo\.com|login\.microsoftonline\.com|\/login|\/sign-in|\/authorize/i.test(url2)) {
    throw new LoginRequired(`bounced to an auth page: ${url2}`);
  }
  if (!listReady) {
    throw new LoginRequired(`expenses list did not render (url: ${url2}); session may have lapsed`);
  }
}

// --------------------------------------------------------------------------
// Form driving
// --------------------------------------------------------------------------

// Dismiss a cookie-consent banner if one overlays the page. On a cold profile
// load an "Accept All" / "Accept" banner can intercept clicks; on a warm profile
// it is absent. Best-effort and silent when nothing is present.
async function dismissConsentBanner(page: Page): Promise<void> {
  try {
    const dismissed = await page.evaluate(() => {
      const nodes = Array.from(document.querySelectorAll("button, a")) as HTMLElement[];
      const b = nodes.find((x) => /^(accept all|accept|allow all|agree|got it|i agree)$/i.test((x.textContent || "").trim()));
      if (b) {
        b.click();
        return true;
      }
      return false;
    });
    if (dismissed) await settle(page, 400);
  } catch {
    /* ignore */
  }
}

async function openForm(page: Page): Promise<void> {
  // A cookie-consent banner can overlay a cold-profile list and intercept the
  // first clicks; dismiss it before touching the Add Expense affordance.
  await dismissConsentBanner(page);
  // Open the "Add Expense" dropdown from the top button. JS-dispatched click as a
  // fallback for overlay intercepts (battle-tested: plain clicks intermittently
  // time out against a transient overlay).
  const topBtn = page.locator("button, cp-button", { hasText: /^\s*Add Expense\s*$/ }).first();
  await topBtn.click({ timeout: T.med }).catch(async () => {
    await page.evaluate(() => {
      const btns = Array.from(document.querySelectorAll("button, cp-button"));
      const b = btns.find((x) => (x.textContent || "").trim() === "Add Expense") as HTMLElement | undefined;
      b?.click();
    });
  });
  await settle(page, 600);
  // Wait for the dropdown overlay's "Add Expense" menu item to render. This item
  // is a <cp-dropdown-item> (NOT a <button>), so it must be matched WITHOUT
  // button/cp-button in the selector - otherwise the top "Add Expense" button
  // (same text, earlier in the DOM) wins .first() and re-clicking it just closes
  // the menu, leaving the form never opened. Click the menu item via JS dispatch
  // (the proven path here), scoped to cp-dropdown-item and NOT to "Upload receipts".
  const menuItem = page.locator("cp-dropdown-item, [role=menuitem]").filter({ hasText: /^\s*Add Expense\s*$/ }).first();
  await menuItem.waitFor({ state: "visible", timeout: T.med });
  const jsClicked = await page.evaluate(() => {
    const items = Array.from(document.querySelectorAll("cp-dropdown-item, [role=menuitem]"));
    const it = items.find((x) => (x.textContent || "").trim() === "Add Expense") as HTMLElement | undefined;
    if (!it) return false;
    const target = (it.querySelector(".cp-dropdown-item__text") as HTMLElement | null) ?? it;
    target.click();
    return true;
  });
  if (!jsClicked) {
    // Fallback to a real Playwright click on the scoped menu item.
    await menuItem.click({ timeout: T.med });
  }
  await page.waitForURL(/new-regular-expense|expenses\/.*new/i, { timeout: T.long }).catch(() => {});
  await amountInput(page).waitFor({ state: "visible", timeout: T.long });
  await settle(page, 600);
}

// Fill the whole form (does not save). Returns the chosen department label.
async function fillForm(page: Page, e: Entry, warnings: string[]): Promise<{ chosenDept: string }> {
  await amountInput(page).fill(e.amount, { timeout: T.med });
  if (e.currency !== "EUR") {
    await pickSelect(page, currencySelect(page), e.currency, [e.currency]);
  }
  await merchantInput(page).fill(e.merchant, { timeout: T.med });
  const di = dateInput(page);
  await di.click({ timeout: T.med });
  await di.fill(e.date, { timeout: T.med });
  await di.press("Enter");
  await settle(page, 400);
  // NOTE: never press Escape here. With no select overlay open, Escape closes the
  // topmost CDK overlay, which is the sidepanel form itself.
  await pickSelect(page, categorySelect(page), e.category, [e.category]);
  await pickSelect(page, projectSelect(page), e.projectFilter, [e.projectLabel]);
  const deptField = await customFieldByLabel(page, "Department");
  const chosenDept = await pickSelect(page, deptField, "", e.departmentPreference);
  if (chosenDept !== e.departmentPreference[0]) {
    warnings.push(`department fell back to "${chosenDept}" (first choice "${e.departmentPreference[0]}" unavailable)`);
  }
  if (e.investor) {
    const invField = await customFieldByLabel(page, "Investor");
    await pickSelect(page, invField, e.investor, [e.investor]);
  }
  const locField = await customFieldByLabel(page, "Location");
  await pickSelect(page, locField, e.location, [e.location]);
  await setComment(page, e.comment);
  // Invoice upload (hidden file input).
  const invoice = resolvePath(e.invoicePath);
  if (!existsSync(invoice)) throw new Error(`invoice not found: ${invoice}`);
  await page.locator('input[type="file"]').first().setInputFiles(invoice, { timeout: T.med });
  await settle(page, 1500);
  return { chosenDept };
}

// Assert filled values took, reading REAL DOM values (not accessibility labels).
async function assertReadbacks(page: Page, e: Entry, entryNo: number): Promise<void> {
  const fail = (step: string, detail: string): never => {
    throw new BatchError(entryNo, e.merchant, step, detail);
  };
  const amount = (await amountInput(page).inputValue()).trim();
  if (!amount || parseFloat(amount.replace(",", ".")) !== parseFloat(e.amount.replace(",", "."))) {
    fail("readback:amount", `form has "${amount}", expected "${e.amount}"`);
  }
  const merchant = (await merchantInput(page).inputValue()).trim();
  if (merchant !== e.merchant) fail("readback:merchant", `form "${merchant}" vs "${e.merchant}"`);
  const dateVal = (await dateInput(page).inputValue()).replace(/\s/g, "");
  if (dateVal.replace(/\//g, "") !== e.date.replace(/\s/g, "").replace(/\//g, "")) {
    fail("readback:date", `form "${dateVal}" vs "${e.date}"`);
  }
  const selectVal = async (testid: string) =>
    (await page.locator(`cp-select[data-testid="${testid}"] input`).first().inputValue()).trim();
  const cur = await selectVal("currency");
  if (!cur.includes(e.currency)) fail("readback:currency", `field shows "${cur}", expected ${e.currency}`);
  const cat = await selectVal("category");
  if (!cat.includes(e.category)) fail("readback:category", `field shows "${cat}", expected "${e.category}"`);
  const proj = await selectVal("project");
  if (!proj.includes(e.projectLabel)) fail("readback:project", `field shows "${proj}", expected "${e.projectLabel}"`);
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
  if (!e.departmentPreference.includes(custom["Department"] ?? "")) {
    fail("readback:department", `shows "${custom["Department"] ?? ""}", expected one of [${e.departmentPreference.join(", ")}]`);
  }
  if (e.investor && (custom["Investor"] ?? "") !== e.investor) {
    fail("readback:investor", `shows "${custom["Investor"] ?? ""}", expected "${e.investor}"`);
  }
  if ((custom["Location"] ?? "") !== e.location) {
    fail("readback:location", `shows "${custom["Location"] ?? ""}", expected "${e.location}"`);
  }
  const comment = await page.evaluate(() => {
    const host = document.querySelector('[data-testid="comment"]');
    const ce = host?.querySelector('[contenteditable], .cp-input__textarea--editable');
    return (ce?.textContent || "").trim();
  });
  if (comment !== e.comment) fail("readback:comment", `shows "${comment}", expected "${e.comment}"`);
  // Assert the invoice attachment is present. After upload Rydoo renders the
  // filename in a receipt-upload label (span.gyr-receipt-upload__pdf-label),
  // NOT a canvas or a cp-list-item. Assert the uploaded file's basename appears
  // in that label (or any receipt/thumbnail container as a fallback), which also
  // proves the CORRECT file attached, not merely that some node exists.
  const invoiceBase = resolvePath(e.invoicePath).split("/").pop() ?? "";
  const attachment = await page.evaluate((base) => {
    const norm = (s: string) => (s || "").replace(/\s+/g, " ").trim();
    const labels = Array.from(
      document.querySelectorAll(
        ".gyr-receipt-upload__pdf-label, [class*='receipt'], [class*='thumbnail'], [class*='attachment'], .cp-list-item__text",
      ),
    );
    const byName = labels.some((n) => norm(n.textContent || "").includes(base));
    // Fallback: a rendered preview canvas/img is also acceptable evidence.
    const hasPreview = document.querySelectorAll(".gyr-receipt-upload canvas, .gyr-receipt-upload img, canvas").length > 0;
    return { byName, hasPreview, sampled: labels.slice(0, 6).map((n) => norm(n.textContent || "").slice(0, 40)) };
  }, invoiceBase);
  if (!attachment.byName && !attachment.hasPreview) {
    fail(
      "readback:invoice",
      `uploaded "${invoiceBase}" not visible after upload (sampled labels: [${attachment.sampled.join(" | ")}])`,
    );
  }
}

// Assert the primary save button reads exactly "Save" (never "Submit"), then click.
async function assertSaveAndClick(page: Page, entryNo: number, merchant: string): Promise<void> {
  const btn = saveButton(page);
  await btn.waitFor({ state: "visible", timeout: T.med });
  const name = (await btn.innerText()).trim();
  if (name !== "Save") {
    throw new BatchError(entryNo, merchant, "save", `refusing to click: button reads "${name}", expected exactly "Save"`);
  }
  await btn.click({ timeout: T.med });
}

async function verifySavedToast(page: Page, warnings: string[]): Promise<void> {
  try {
    await page.locator("text=/Expense saved/i").first().waitFor({ state: "visible", timeout: T.short });
  } catch {
    warnings.push('did not observe "Expense saved" toast (may have auto-dismissed)');
  }
}

// Apply the EUR override on the reopened row, attach the bank proof, save, and
// read back [data-testid=converted-amount]. Asserts it contains the override.
async function applyEurOverride(page: Page, e: Entry, entryNo: number): Promise<{ verifiedEur: string; attachments: number }> {
  const override = e.eurOverride as string;
  await page.waitForURL(/expenses\/personal(\/)?$/, { timeout: T.long }).catch(() => {});
  await settle(page, 800);
  const rowSel = () => page.locator("cp-table-row, [role=row], tr").filter({ hasText: e.merchant }).filter({ hasText: e.date }).first();
  await rowSel().waitFor({ state: "visible", timeout: T.med });
  await rowSel().click({ timeout: T.med });
  await settle(page, 1200);
  const pencil = page.locator('[data-testid="edit-exchange-rate-button"]').first();
  await pencil.waitFor({ state: "visible", timeout: T.med });
  await pencil.click({ timeout: T.med });
  const dialog = page.locator(".cdk-overlay-pane, [role=dialog]").filter({ hasText: /Converted amount/i }).first();
  await dialog.waitFor({ state: "visible", timeout: T.med });
  const dInput = dialog.locator("input").first();
  await dInput.fill(override, { timeout: T.med });
  await dialog.locator("button", { hasText: /^\s*Save\s*$/ }).first().click({ timeout: T.med });
  await settle(page, 800);
  let attachments = 1;
  if (e.bankProofPath) {
    const proof = resolvePath(e.bankProofPath);
    if (!existsSync(proof)) throw new Error(`bank proof not found: ${proof}`);
    await page.locator('input[type="file"]').last().setInputFiles(proof, { timeout: T.med });
    await settle(page, 1500);
    attachments = 2;
  }
  await assertSaveAndClick(page, entryNo, e.merchant);
  await settle(page, 1500);
  // Reopen and assert the converted amount contains the override value.
  await page.waitForURL(/expenses\/personal(\/)?$/, { timeout: T.long }).catch(() => {});
  await settle(page, 800);
  await rowSel().click({ timeout: T.med });
  await settle(page, 1200);
  const conv = page.locator('[data-testid="converted-amount"]').first();
  await conv.waitFor({ state: "visible", timeout: T.med });
  const convText = (await conv.innerText()).trim();
  const normOverride = override.replace(".", ",");
  if (!convText.includes(normOverride)) {
    throw new BatchError(entryNo, e.merchant, "verify:eurOverride", `converted-amount "${convText}" does not contain "${normOverride}"`);
  }
  // Close the panel without saving again.
  await cancelButton(page).click({ timeout: T.short }).catch(async () => {
    await page.keyboard.press("Escape").catch(() => {});
  });
  await settle(page, 600);
  return { verifiedEur: convText, attachments };
}

// Best-effort: leave no form open. Cancel, else Escape.
async function safeCancel(page: Page): Promise<void> {
  try {
    await closeSelectOverlay(page);
    if (await cancelButton(page).isVisible({ timeout: 1000 })) {
      await cancelButton(page).click({ timeout: T.short });
      await page.waitForURL(/expenses\/personal(\/)?$/, { timeout: T.med }).catch(() => {});
    }
  } catch {
    try {
      await page.keyboard.press("Escape");
    } catch {
      /* ignore */
    }
  }
}

// Process a single entry. In dry-run: fill + upload + readback assertions, then
// Cancel and assert the list count is unchanged. Live: fill, save, and (for USD)
// apply the override.
async function processEntry(page: Page, e: Entry, entryNo: number, dryRun: boolean): Promise<EntryResult> {
  const warnings: string[] = [];
  const result: EntryResult = {
    merchant: e.merchant,
    date: e.date,
    amount: e.amount,
    currency: e.currency,
    saved: false,
    verifiedEur: null,
    attachments: 0,
    warnings,
  };
  let step = "start";
  try {
    const beforeCount = await listRowCount(page);
    step = "openForm";
    await openForm(page);
    step = "fillForm";
    await fillForm(page, e, warnings);
    step = "readback";
    await assertReadbacks(page, e, entryNo);

    if (dryRun) {
      step = "dryrun-cancel";
      await safeCancel(page);
      await assertOnAuthenticatedList(page);
      const afterCount = await listRowCount(page);
      if (afterCount !== beforeCount) {
        throw new BatchError(entryNo, e.merchant, "dryrun-cancel", `list row count changed ${beforeCount} -> ${afterCount} (a draft may have been created)`);
      }
      result.attachments = 1; // uploaded in-form, discarded on cancel
      warnings.push(`dry-run: form cancelled, list unchanged (${afterCount} rows)`);
      return result;
    }

    step = "save";
    await closeSelectOverlay(page);
    await assertSaveAndClick(page, entryNo, e.merchant);
    await verifySavedToast(page, warnings);
    result.saved = true;
    result.attachments = 1;

    if (e.eurOverride) {
      step = "eurOverride";
      const { verifiedEur, attachments } = await applyEurOverride(page, e, entryNo);
      result.verifiedEur = verifiedEur;
      result.attachments = attachments;
    }
    return result;
  } catch (err) {
    await safeCancel(page);
    if (err instanceof BatchError) throw err;
    const raw = (err as Error).message || String(err);
    throw new BatchError(entryNo, e.merchant, step, raw.split("\n")[0].trim());
  }
}

// --------------------------------------------------------------------------
// Arg parsing + main
// --------------------------------------------------------------------------

function parseArgs(argv: string[]): { flags: Record<string, string | boolean> } {
  const flags: Record<string, string | boolean> = {};
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      const key = a.slice(2);
      const next = argv[i + 1];
      if (next === undefined || next.startsWith("--")) flags[key] = true;
      else {
        flags[key] = next;
        i++;
      }
    }
  }
  return { flags };
}

function usage(): void {
  process.stdout.write(
    [
      "rydoo-batch - deterministic monthly Rydoo draft creation (zero LLM calls)",
      "",
      "  node dist/rydoo-batch.js --spec <entries.json> [--dry-run] [--entry <n>] [--profile <dir>] [--email <addr>] [--foreground]",
      "",
      "  --spec <path>     JSON array of entries (see examples/entries-template.json)",
      "  --dry-run         fill + upload + assert readbacks, then Cancel; nothing saved",
      "  --entry <n>       process only the 1-based nth entry",
      "  --profile <dir>   persistent profile dir (default ~/.cache/pw-browse-rydoo)",
      "  --email <addr>    SSO email for the silent login bounce (default: $RYDOO_EMAIL)",
      "  --foreground      keep the browser window visible (default: minimized so it never steals focus)",
      "",
      "Never clicks Submit. Halts loudly on the first failed assertion.",
    ].join("\n") + "\n",
  );
}

async function main(): Promise<void> {
  const { flags } = parseArgs(process.argv.slice(2));
  if (flags.help || flags.h || (!flags.spec && process.argv.slice(2).length === 0)) {
    usage();
    process.exit(flags.help || flags.h ? 0 : 1);
  }
  const specPath = flags.spec as string;
  if (!specPath || typeof specPath !== "string") {
    process.stderr.write("error: --spec <entries.json> is required\n");
    process.exit(1);
  }
  const dryRun = flags["dry-run"] === true || flags.dryRun === true;
  const onlyEntry = typeof flags.entry === "string" ? parseInt(flags.entry, 10) : null;
  const profileDir = typeof flags.profile === "string" ? resolvePath(flags.profile) : DEFAULT_PROFILE;
  const email = typeof flags.email === "string" && flags.email.length > 0 ? flags.email : ENV_EMAIL;
  if (!email) {
    process.stderr.write("error: no SSO email; pass --email <addr> or set the RYDOO_EMAIL env var\n");
    process.exit(1);
  }
  const foreground = flags.foreground === true;

  let entries: Entry[];
  try {
    entries = loadSpec(specPath);
  } catch (e) {
    process.stderr.write(`error: ${(e as Error).message}\n`);
    process.exit(1);
  }
  if (onlyEntry !== null) {
    if (!Number.isInteger(onlyEntry) || onlyEntry < 1 || onlyEntry > entries.length) {
      process.stderr.write(`error: --entry ${flags.entry} out of range (1..${entries.length})\n`);
      process.exit(1);
    }
    entries = [entries[onlyEntry - 1]];
  }

  process.stdout.write(`rydoo-batch: ${dryRun ? "DRY RUN" : "LIVE"}, ${entries.length} entr${entries.length === 1 ? "y" : "ies"}, profile ${profileDir}\n`);

  let context: BrowserContext | null = null;
  let exitCode = 0;
  try {
    context = await chromium.launchPersistentContext(profileDir, {
      headless: false,
      args: [
        "--disable-background-timer-throttling",
        "--disable-backgrounding-occluded-windows",
        "--disable-renderer-backgrounding",
        "--disable-features=CalculateNativeWinOcclusion",
      ],
      viewport: null,
    });
    const page = context.pages()[0] ?? (await context.newPage());

    // By default launch unobtrusively: minimize the window immediately so it
    // never pops in front of the user and steals keyboard focus. --foreground
    // keeps it visible (needed for the manual MFA handoff, where the user must
    // interact with the Microsoft login page).
    if (foreground) {
      process.stdout.write("foreground mode: window stays visible\n");
    } else {
      const minimized = await minimizeWindow(context, page);
      if (minimized) process.stdout.write("window minimized (no focus steal); pass --foreground to keep it visible\n");
    }

    // Navigate to the list and assert we are authenticated.
    await page.goto(RYDOO_LIST_URL, { waitUntil: "domcontentloaded", timeout: T.long }).catch(() => {});
    await settle(page, 1000);
    // A fresh browser start always lands on the Rydoo login page (Rydoo's own
    // session cookie is not persistent). Try the silent SSO bounce first; if it
    // fails, the URL stays on an auth page and assertOnAuthenticatedList below
    // raises the LOGIN REQUIRED handoff.
    if (/accounts\.rydoo\.com/i.test(page.url())) {
      await attemptSilentSsoBounce(page, email);
    }
    try {
      await assertOnAuthenticatedList(page);
    } catch (e) {
      if (e instanceof LoginRequired) {
        process.stderr.write(
          [
            "",
            "LOGIN REQUIRED: not on the authenticated Rydoo expenses list.",
            `  detail: ${e.message}`,
            "  The Entra session has lapsed. Run the manual MFA handoff first:",
            `    1. Open this same profile headed and complete Microsoft SSO + MFA once,`,
            `       ticking 'stay signed in' so the Entra cookie is persistent.`,
            `       e.g. open a browser on ${profileDir} and log in to ${RYDOO_LIST_URL}`,
            "    2. Re-run this command. Never attempt to automate the MFA challenge.",
            "",
          ].join("\n") + "\n",
        );
        exitCode = 2;
        return;
      }
      throw e;
    }

    const results: EntryResult[] = [];
    for (let i = 0; i < entries.length; i++) {
      const entryNo = onlyEntry !== null ? onlyEntry : i + 1;
      const e = entries[i];
      process.stdout.write(`\n[entry ${entryNo}/${onlyEntry !== null ? entryNo : entries.length}] ${e.merchant} ${e.amount} ${e.currency} ${e.date}\n`);
      // Re-assert list state before each entry (a prior save returns us here).
      await assertOnAuthenticatedList(page);
      const res = await processEntry(page, e, entryNo, dryRun);
      results.push(res);
      const tag = dryRun ? "dry-run OK" : res.saved ? "saved" : "not saved";
      const eur = res.verifiedEur ? `, EUR=${res.verifiedEur}` : "";
      process.stdout.write(`  ${tag}, attachments=${res.attachments}${eur}\n`);
      for (const w of res.warnings) process.stdout.write(`  warn: ${w}\n`);
    }

    process.stdout.write(`\n${dryRun ? "DRY RUN complete" : "LIVE run complete"}: ${results.length} entr${results.length === 1 ? "y" : "ies"} processed, 0 failures.\n`);
  } catch (err) {
    exitCode = 1;
    const msg = err instanceof BatchError ? err.message : (err as Error).message || String(err);
    process.stderr.write(`\nFAILED: ${msg}\n`);
  } finally {
    if (context) {
      try {
        await context.close();
      } catch {
        /* ignore */
      }
    }
    // Exit from the finally so an early `return` inside the try (the login-required
    // handoff) still propagates the intended nonzero exit code.
    process.exit(exitCode);
  }
}

void main();
