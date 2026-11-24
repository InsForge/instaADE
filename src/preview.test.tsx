import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import { StrictMode } from "react";

vi.mock("./hero/HeroCube", () => ({ HeroCube: () => null }));

vi.mock("@tauri-apps/api/core", () => ({
  isTauri: () => false,
  invoke: () => { throw new Error("Browser preview must not call native APIs"); },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: () => { throw new Error("Browser preview must not listen to native events"); },
}));

import App from "./App";
import { connectSandbox, getState, openInAde, pause } from "./api";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

test("browser preview renders, navigates and updates sample data without native APIs", async () => {
  render(<StrictMode><App /></StrictMode>);
  await userEvent.click(await screen.findByText("dev-workspace"));
  expect(screen.getByRole("button", { name: "Open in T3 Code" })).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "Back to sandboxes" }));
  const open = vi.spyOn(window, "open").mockReturnValue(null);
  await userEvent.click(screen.getByRole("button", { name: "Open InstaBox dashboard" }));
  expect(open).toHaveBeenCalledWith("https://dashboard.instabox.sh/dashboard/machines", "_blank", "noopener,noreferrer");
  await act(() => pause("a"));
  expect(screen.getByRole("button", { name: "Resume dev-workspace" })).toBeTruthy();
  await userEvent.click(screen.getByRole("combobox", { name: "Workspace" }));
  await userEvent.click(screen.getByRole("option", { name: "org-team" }));
  expect(screen.getByRole("combobox", { name: "Workspace" }).textContent).toBe("org-team");
  await expect(openInAde("a", "t3")).rejects.toMatchObject({ kind: "other", message: expect.stringContaining("browser preview") });
});

test("preview update and connect actions preserve other connected ADEs", async () => {
  render(<App />);
  await screen.findByText("website");
  const website = within(screen.getByText("website").closest("li")!);
  await userEvent.click(website.getByRole("button", { name: "Update" }));
  expect(website.queryByRole("button", { name: "Update" })).toBeNull();
  const design = within(screen.getByText("design-system").closest("li")!);
  expect(design.getByRole("img", { name: "cmux" })).toBeTruthy();
  await userEvent.click(design.getByRole("button", { name: "Connect" }));
  expect(design.queryByRole("button", { name: "Connect" })).toBeNull();
  expect(design.getByRole("img", { name: "cmux" })).toBeTruthy();
  expect(design.getByRole("img", { name: "T3 Code" })).toBeTruthy();
});

test("preview shows every ADE: all four in the switcher, Orca and Paradev on a sample sandbox", async () => {
  render(<App />);
  await screen.findByText("mobile-app");
  const mobile = within(screen.getByText("mobile-app").closest("li")!);
  expect(mobile.getByRole("img", { name: "Orca" })).toBeTruthy();
  expect(mobile.getByRole("img", { name: "Paradev" })).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: /Switch ADE/ }));
  const names = screen.getAllByRole("menuitemradio").map((item) => item.querySelector(".strong")?.textContent);
  expect(names).toEqual(["InstaBox", "T3 Code", "cmux", "Orca", "Paradev"]);
});

test("preview Install Paradev hands the setup over and leaves the sandbox's mark as it was", async () => {
  const before = (await getState()).sandboxes.find((s) => s.name === "api-server")!;
  expect(before.ades).toEqual(["t3", "cmux"]);
  await expect(connectSandbox(before.id, "paradev")).resolves.toBe(
    "The SSH address is copied. In Paradev, click Set up a host over SSH and paste it.",
  );
  const after = (await getState()).sandboxes.find((s) => s.id === before.id)!;
  expect(after.ades).toEqual(["t3", "cmux"]);
});
