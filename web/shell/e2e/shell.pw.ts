// The default shell against a real server: the setup link, the home page, and
// the page a plugin adds.
import { expect, test } from "@playwright/test";

/** setupLink is the first admin's one-time link, made by the Go test. */
const setupLink = process.env['ROOKERY_SETUP_LINK'] ?? "";

test("a first admin signs in with the setup link and sees the shell", async ({ page }) => {
  expect(setupLink).not.toBe("");

  await page.goto(setupLink);
  await page.getByLabel("Email").fill("operator@example.test");
  await page.getByLabel("Name").fill("Shell Operator");
  await page.getByRole("button", { name: "Create the admin" }).click();

  // The setup link signs the browser in and sends it to the shell: Nocturne
  // draws the chrome, the home view greets the admin, and the status bar stays
  // quiet while the link to the server is up.
  await expect(page.locator("[data-part='app-shell']")).toBeVisible();
  await expect(page.locator("[data-view='home']")).toContainText("Welcome, Shell Operator");
  await expect(page.locator("[data-part='connection-state']")).toHaveCount(0);

  // The navigation is built from the core's pages.
  await expect(page.getByRole("link", { name: "Home", exact: true })).toBeVisible();

  // A page of the server's own opens from it.
  await page.getByRole("link", { name: "Plugins", exact: true }).click();
  await expect(page.locator("[data-part='view-header']")).toContainText("Plugins");
  await page.getByRole("button", { name: "Install plugin" }).first().click();
  await expect(page.getByRole("dialog")).toBeVisible();
});
