// Shared wire protocol and filesystem paths for the pw-browse daemon and client.
// Messages are newline-delimited JSON (NDJSON) over a unix domain socket.

import { homedir } from "node:os";
import { join } from "node:path";

// State directory holds the socket, pid file, and log. Keep it out of the repo.
export const STATE_DIR = join(homedir(), ".pw-browse");
export const SOCK_PATH = join(STATE_DIR, "daemon.sock");
export const PID_PATH = join(STATE_DIR, "daemon.pid");
export const LOG_PATH = join(STATE_DIR, "daemon.log");
export const META_PATH = join(STATE_DIR, "daemon.json");

export interface Request {
  verb: string;
  args: Record<string, unknown>;
}

export interface Response {
  ok: boolean;
  // Present when ok. Free-form per verb.
  data?: unknown;
  // Present when !ok. One-line human message.
  error?: string;
}
