// Loaded into `t3 serve` on an instabox sandbox through NODE_OPTIONS (t3-instabox.service).
//
// The instabox port proxy sends a request to T3's published port 3774 with
// `Host: 127.0.0.1:3774`, and the address the client used in `X-Forwarded-Host`
// (`3774-<sandbox id>.<domain>`), which it always sets itself. T3 builds the request
// URL from Host, and since DPoP (T3 0.1.12) every proof must name that URL, so a client
// that signs the public address gets "request_mismatch". For a request that came
// through the proxy, this puts the public host in Host before T3 reads it.
"use strict";

const fs = require("node:fs");
const http = require("node:http");

const PORT = 3774;
const LOCAL = `127.0.0.1:${PORT}`;
const PUBLIC = new RegExp(`^${PORT}-([a-z0-9]+)(\\.[a-z0-9-]+)+$`);
const IDENTITY = "/etc/instabox/identity";

// T3's own children (terminals, agents, the user's dev servers) must not load this file.
function dropOwnOption() {
  const options = process.env.NODE_OPTIONS;
  if (options === undefined) return;
  const own = (o) => {
    if (!o.startsWith("--require=")) return false;
    try {
      return fs.realpathSync(o.slice("--require=".length)) === __filename;
    } catch {
      return false;
    }
  };
  const kept = options.split(/\s+/).filter((o) => o !== "" && !own(o));
  if (kept.length > 0) process.env.NODE_OPTIONS = kept.join(" ");
  else delete process.env.NODE_OPTIONS;
}

// The label of this sandbox's public hosts (its id without "m_"); null when the
// sandbox has no identity stamp.
function ownLabel() {
  try {
    const id = fs.readFileSync(IDENTITY, "utf8").trim().toLowerCase();
    if (id === "") return null;
    return id.startsWith("m_") ? id.slice(2) : id;
  } catch {
    return null;
  }
}

let label = ownLabel();

// The public host of a request that came through the proxy to this port, else null.
function publicHost(headers) {
  if (headers.host !== LOCAL) return null;
  const forwarded = headers["x-forwarded-host"];
  if (typeof forwarded !== "string") return null;
  const host = forwarded.toLowerCase();
  const m = PUBLIC.exec(host);
  if (m === null) return null;
  if (label !== null && m[1] !== label) {
    // A clone gets a new identity: read it again before refusing.
    label = ownLabel();
    if (label !== null && m[1] !== label) return null;
  }
  return host;
}

function fix(req) {
  const host = publicHost(req.headers);
  if (host === null) return;
  req.headers.host = host;
  const raw = req.rawHeaders;
  for (let i = 0; i + 1 < raw.length; i += 2) {
    if (raw[i].toLowerCase() === "host") raw[i + 1] = host;
  }
}

dropOwnOption();

const emit = http.Server.prototype.emit;
http.Server.prototype.emit = function (event, req, ...rest) {
  if ((event === "request" || event === "upgrade") && req && req.headers) {
    try {
      fix(req);
    } catch {
      // Never stop a request: without the fix it is only the old behavior.
    }
  }
  return emit.call(this, event, req, ...rest);
};
