#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");

const CONTRACTION = /\b(i'll|i've|i'm|i'd)\b/;

function walk(target, out) {
  const st = fs.statSync(target);
  if (st.isDirectory()) {
    for (const name of fs.readdirSync(target)) {
      if (name === "." || name === "..") continue;
      walk(path.join(target, name), out);
    }
    return;
  }
  if (st.isFile()) out.push(target);
}

function prose(text) {
  return text.replace(/```[\s\S]*?```/g, "").replace(/`[^`]*`/g, "");
}

const roots = process.argv.slice(2);
if (roots.length === 0) {
  console.error("usage: docs_prose_check.js <path>...");
  process.exit(2);
}

let failed = false;
for (const root of roots) {
  const files = [];
  walk(root, files);
  for (const file of files) {
    const text = fs.readFileSync(file, "utf8");
    const lines = prose(text).split(/\r?\n/);
    lines.forEach((line, idx) => {
      if (CONTRACTION.test(line)) {
        console.error(`${file}:${idx + 1}: capitalize first-person contractions in docs`);
        failed = true;
      }
    });
  }
}

process.exit(failed ? 1 : 0);
