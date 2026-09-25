/**
 * Host-only ESLint config for Codex Warp browser scripts.
 * Paths are relative to the repository root.
 */
import js from "@eslint/js";
import { defineConfig } from "eslint/config";
import globals from "globals";

export default defineConfig([
    {
        files: ["src/webui_static/**/*.js"],
        extends: [js.configs.recommended],
        languageOptions: {
            ecmaVersion: 2022,
            sourceType: "script",
            globals: {
                ...globals.browser,
                // chart-math.js and footer-status.js assign module.exports
                // when a Node harness loads the same browser file.
                module: "readonly",
            },
        },
        rules: {
            // Existing pages assign let data = null before a try that always
            // replaces it, and theme-bootstrap.js catches storage errors it
            // does not read.
            "no-useless-assignment": "off",
            "no-unused-vars": ["error", {
                caughtErrors: "none",
            }],
        },
    },
]);
