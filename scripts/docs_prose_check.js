#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");

const CONTRACTION = /\b(i['’]ll|i['’]ve|i['’]m|i['’]d)\b/;
const BASE_DIR = fs.realpathSync(path.resolve(__dirname, ".."));
const SAFE_SEGMENT = /^[A-Za-z0-9._-]+$/;
const visitedDirectories = new Set();

function isWithinBase(target) {
  return (
    typeof target === "string" &&
    target.startsWith(BASE_DIR) &&
    (target === BASE_DIR || target.startsWith(`${BASE_DIR}${path.sep}`))
  );
}

function hasSafePathSegments(target) {
  if (!isWithinBase(target) || target === BASE_DIR) return target === BASE_DIR;
  return target
    .slice(BASE_DIR.length + 1)
    .split(path.sep)
    .every((segment) => segment !== "." && segment !== ".." && SAFE_SEGMENT.test(segment));
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
  if (resolved.startsWith(BASE_DIR) && isWithinBase(resolved)) {
    const real = fs.realpathSync(resolved);
    if (real.startsWith(BASE_DIR) && isWithinBase(real) && hasSafePathSegments(real)) {
      return resolved;
    }
  }
  throw new Error(`documentation path escapes repository: ${input}`);
}

function resolveChild(target, name) {
  if (
    typeof target !== "string" ||
    !path.isAbsolute(target) ||
    !target.startsWith(BASE_DIR) ||
    !isWithinBase(target) ||
    typeof name !== "string" ||
    name === "." ||
    name === ".." ||
    path.isAbsolute(name) ||
    !SAFE_SEGMENT.test(name)
  ) {
    throw new Error(`unsafe documentation path: ${name}`);
  }

  const relativeParent = path.relative(BASE_DIR, target);
  if (relativeParent.startsWith("..") || path.isAbsolute(relativeParent)) {
    throw new Error(`documentation path escapes repository: ${name}`);
  }

  const resolved = path.resolve(BASE_DIR, relativeParent, name);
  if (resolved.startsWith(BASE_DIR) && isWithinBase(resolved)) {
    const real = fs.realpathSync(resolved);
    if (real.startsWith(BASE_DIR) && isWithinBase(real) && hasSafePathSegments(real)) {
      return resolved;
    }
  }
  throw new Error(`documentation path escapes repository: ${name}`);
}

function walk(target, out) {
  if (
    typeof target !== "string" ||
    !path.isAbsolute(target) ||
    !target.startsWith(BASE_DIR) ||
    !isWithinBase(target)
  ) {
    throw new Error(`documentation path escapes repository: ${target}`);
  }

  const realTarget = fs.realpathSync(target);
  if (!realTarget.startsWith(BASE_DIR) || !isWithinBase(realTarget)) {
    throw new Error(`documentation path escapes repository: ${target}`);
  }
  if (!hasSafePathSegments(realTarget)) {
    throw new Error(`unsafe documentation path: ${target}`);
  }

  if (realTarget.startsWith(BASE_DIR)) {
    const st = fs.statSync(realTarget);
    if (st.isDirectory()) {
      if (visitedDirectories.has(realTarget)) return;
      visitedDirectories.add(realTarget);
      for (const name of fs.readdirSync(realTarget)) {
        if (name === "." || name === "..") continue;
        walk(resolveChild(target, name), out);
      }
      return;
    }
    if (st.isFile() && target.endsWith(".md")) {
      out.push({ readPath: realTarget, displayPath: target });
    }
    return;
  }

  throw new Error(`documentation path escapes repository: ${target}`);
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
      const text = fs.readFileSync(file.readPath, "utf8");
      const lines = prose(text).split(/\r?\n/);
      lines.forEach((line, idx) => {
        if (CONTRACTION.test(line)) {
          console.error(
            `${file.displayPath}:${idx + 1}: capitalize first-person contractions in docs`
          );
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
