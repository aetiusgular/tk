import { defineConfig } from "vitest/config";

// Only *.test.ts runs. A *.test.tsx or *.test.js file is not collected:
// the scope script refuses new ones so nothing is skipped without a word.
export default defineConfig({
  test: {
    include: ["packages/*/**/*.test.ts", "library/**/*.test.ts", "contracts/**/*.test.ts"],
    exclude: ["**/node_modules/**", "**/dist/**"],
  },
});
