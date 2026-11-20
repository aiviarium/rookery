import { defineConfig, devices } from "@playwright/test";

// The shell's end-to-end test runs against a real `rookery serve`, which the Go
// test in e2e/shell_test.go starts and passes in `ROOKERY_SHELL_URL`.
export default defineConfig({
  testDir: "./e2e",
  testMatch: "**/*.pw.ts",
  timeout: 30_000,
  fullyParallel: false,
  workers: 1,
  reporter: "list",
  use: {
    trace: "retain-on-failure",
    ...devices["Desktop Chrome"],
  },
});
