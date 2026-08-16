#!/usr/bin/env node
"use strict";

const fs = require("fs");

function rangesFromDiff(text) {
  const ranges = [];
  let file = null;
  for (const line of text.split(/\r?\n/)) {
    const plus = /^\+\+\+ (?:b\/)?(.+)$/.exec(line);
    if (plus) {
      file = plus[1] === "/dev/null" ? null : plus[1].replace(/\\/g, "/");
      continue;
    }
    const hunk = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (!hunk || !file || !file.endsWith(".rs")) continue;
    const start = Number(hunk[1]);
    const count = hunk[2] === undefined ? 1 : Number(hunk[2]);
    if (count === 0) continue;
    ranges.push({ file, start, end: start + count - 1 });
  }
  return ranges;
}

function fileMatches(spanFile, diffFile) {
  const file = spanFile.replace(/\\/g, "/");
  return (
    file === diffFile ||
    file.endsWith(`/${diffFile}`) ||
    file.endsWith(diffFile)
  );
}

const diffPath = process.argv[2];
if (!diffPath) {
  console.error("usage: filter_clippy_changed.js <unified-diff-file>");
  process.exit(2);
}

const ranges = rangesFromDiff(fs.readFileSync(diffPath, "utf8"));
if (ranges.length === 0) process.exit(0);

let failed = false;
for (const line of fs.readFileSync(0, "utf8").split(/\r?\n/)) {
  if (!line) continue;
  let msg;
  try {
    const parsed = JSON.parse(line);
    if (parsed.reason !== "compiler-message") continue;
    msg = parsed.message;
  } catch {
    continue;
  }
  if (!msg || (msg.level !== "warning" && msg.level !== "error")) continue;
  const spans = (msg.spans || []).filter((span) => span && span.file_name);
  if (spans.length === 0) continue;
  const hit = spans.some((span) => {
    const lineStart = Number(span.line_start || 0);
    const lineEnd = Number(span.line_end || lineStart);
    return ranges.some(
      (range) =>
        fileMatches(String(span.file_name), range.file) &&
        lineEnd >= range.start &&
        lineStart <= range.end
    );
  });
  if (!hit) continue;
  const span = spans.find((item) => item.is_primary) || spans[0];
  const lineStart = Number(span.line_start || 0);
  const code = msg.code && msg.code.code ? `${msg.code.code}: ` : "";
  console.error(`${span.file_name}:${lineStart}: ${code}${msg.message}`);
  failed = true;
}

process.exit(failed ? 1 : 0);
