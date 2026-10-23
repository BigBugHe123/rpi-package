import { type Page } from "@playwright/test";

/** Select an option through the custom menu, just as a browser user would. */
export async function chooseMenuOption(
  page: Page,
  name: string,
  value: string | { label: string },
) {
  const labels: Record<string, string> = {
    en: "English",
    zh: "简体中文",
    high: "high",
    offline: "offline",
  };
  const label =
    typeof value === "object"
      ? value.label
      : value === ""
        ? name === "Filter by tag"
          ? "All tags"
          : "All statuses"
        : (labels[value] ?? value);
  await page.getByRole("button", { name, exact: true }).click();
  await page
    .getByRole("listbox", { name, exact: true })
    .getByRole("option", { name: label, exact: true })
    .click();
}
