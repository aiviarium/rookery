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

  // The setup link signs the browser in and sends it to the shell. The page's
  // name is the last crumb of its view header, and Nocturne draws the chrome.
  await expect(page.locator("[data-part='view-header']")).toContainText("Welcome, Shell Operator");
  await expect(page.locator("[data-part='app-shell']")).toBeVisible();
  await expect(page.locator(".nc-shell__status")).toContainText("Connected");

  // The navigation is built from the core's pages.
  await expect(page.getByRole("link", { name: "Home", exact: true })).toBeVisible();

  // A page of the server's own opens from it.
  await page.getByRole("link", { name: "Plugins", exact: true }).click();
  await expect(page.locator("[data-part='view-header']")).toContainText("Plugins");
  await expect(page.getByLabel("Reference")).toBeVisible();
});
