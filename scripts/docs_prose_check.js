#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");

const CONTRACTION = /\b(i['’]ll|i['’]ve|i['’]m|i['’]d)\b/;
const BASE_DIR = fs.realpathSync(path.resolve(__dirname, ".."));
const SAFE_SEGMENT = /^[A-Za-z0-9._-]+$/;
const visitedDirectories = new Set();

function isWithinBase(target) {
  return target === BASE_DIR || target.startsWith(`${BASE_DIR}${path.sep}`);
}

function resolveWithinBase(input) {
  const normalizedInput = typeof input === "string" ? input.replace(/\\/g, "/") : "";
  const segments = normalizedInput.split("/").filter((segment) => segment !== "");
  if (
    (typeof input === "string" &&
      (path.isAbsolute(input) || path.isAbsolute(normalizedInput))) ||
    segments.length === 0 ||
    segments.some(
      (segment) =>
        segment === ".." ||
        (segment !== "" && segment !== "." && !SAFE_SEGMENT.test(segment))
    )
  ) {
    throw new Error(`unsafe documentation path: ${input}`);
  }

  const resolved = path.resolve(BASE_DIR, segments.join("/"));
  if (!isWithinBase(resolved)) {
    throw new Error(`documentation path escapes repository: ${input}`);
  }

  const real = fs.realpathSync(resolved);
  if (!isWithinBase(real)) {
    throw new Error(`documentation path escapes repository: ${input}`);
  }
  return resolved;
}

function resolveChild(target, name) {
  const resolved = path.resolve(target, name);
  if (!isWithinBase(resolved)) {
    throw new Error(`documentation path escapes repository: ${name}`);
  }

  const real = fs.realpathSync(resolved);
  if (!isWithinBase(real)) {
    throw new Error(`documentation path escapes repository: ${name}`);
  }
  return resolved;
}

function walk(target, out) {
  const st = fs.statSync(target);
  if (st.isDirectory()) {
    const realTarget = fs.realpathSync(target);
    if (visitedDirectories.has(realTarget)) return;
    visitedDirectories.add(realTarget);
    for (const name of fs.readdirSync(target)) {
      if (name === "." || name === "..") continue;
      walk(resolveChild(target, name), out);
    }
    return;
  }
  if (st.isFile() && target.endsWith(".md")) out.push(target);
}

function prose(text) {
  return text
    .replace(/```[\s\S]*?```/g, (block) => block.replace(/[^\n]/g, ""))
    .replace(/`[^`]*`/g, "");
}

const roots = process.argv.slice(2);
if (roots.length === 0) {
  console.error("usage: docs_prose_check.js <path>...");
  process.exit(2);
}

let failed = false;
try {
  for (const root of roots) {
    const files = [];
    walk(resolveWithinBase(root), files);
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
} catch (error) {
  console.error(`docs_prose_check: ${error.message}`);
  process.exit(2);
}

process.exit(failed ? 1 : 0);
