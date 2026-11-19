import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import fixture from "./fixtures/state.json";
import type { AppState, SandboxView } from "./types";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));
let emit: (s: AppState) => void = () => {};
vi.mock("@tauri-apps/api/event", () => ({
  listen: (_: string, cb: (e: { payload: AppState }) => void) => {
    emit = (s) => cb({ payload: s });
    return Promise.resolve(() => {});
  },
}));

import App from "./App";

/** Typed literal: tsc checks every nested field against AppState. */
const sample: AppState = {
  signedIn: true,
  login: "alex",
  signInCode: null,
  orgs: [{ id: "org-me", name: "org-me", tier: "free", personal: true }],
  orgId: "org-me",
  tier: "free",
  t3: "0.1.10",
  sandboxes: [],
  sizes: [
    { vcpus: 2, memMiB: 4096, label: "2 vCPU / 4 GB" },
    { vcpus: 4, memMiB: 8192, label: "4 vCPU / 8 GB" },
  ],
  sleepChoices: [{ seconds: 1800, label: "After 30 min" }],
  snapshots: [{ id: "s1", name: "snap-s1" }],
  mcp: { claudeCode: true, codex: false },
  progress: null,
  error: null,
  startAtLogin: false,
  update: null,
};

const box: SandboxView = {
  id: "a",
  name: "dev-1",
  state: "running",
  size: "4 vCPU / 8 GB",
  connected: true,
  t3Version: "0.1.10",
  free: true,
  autoSleep: { seconds: 1800, label: "After 30 min" },
  runLeftMinutes: 80,
  cooldownMinutes: null,
  updatePending: false,
  desktopBehind: false,
};

function show(over: Partial<AppState>, sandboxes: Partial<SandboxView>[] = []) {
  const state: AppState = { ...sample, ...over, sandboxes: sandboxes.map((s) => ({ ...box, ...s })) };
  invoke.mockImplementation((cmd: string) => {
    if (cmd === "get_state") return Promise.resolve(state);
    if (cmd === "agent_status") return Promise.resolve(false);
    if (cmd === "ssh_command") return Promise.resolve("ssh dev-1@ssh.instabox.sh");
    return Promise.resolve();
  });
  return render(<App />);
}

beforeEach(() => invoke.mockReset());
afterEach(cleanup);

test("fixture and sample have the same nested keys", () => {
  const keys = (v: unknown, p = ""): string[] =>
    Array.isArray(v)
      ? v.length
        ? keys(v[0], p + "[]")
        : [p + "[]"]
      : v && typeof v === "object"
        ? Object.entries(v).flatMap(([k, x]) => keys(x, `${p}.${k}`))
        : [p];
  const sampleWithRows: AppState = {
    ...sample,
    sandboxes: [box],
    progress: { kind: "create", step: "x", machineId: null, error: null },
    error: { kind: "other", message: "" },
    update: { version: "", url: "" },
  };
  // minutes and code are optional on ErrorView.
  const strip = (ks: string[]) => [...new Set(ks.filter((k) => !/\.(minutes|code)$/.test(k)))].sort();
  const want = strip(keys(sampleWithRows));
  const got = strip(keys(fixture));
  expect(got.filter((k) => !want.includes(k))).toEqual([]);
  expect(want.filter((k) => !got.includes(k))).toEqual([]);
});

test("free plan: size and auto sleep are disabled and fixed", async () => {
  show({ tier: "free" });
  await userEvent.click(await screen.findByRole("button", { name: "Create sandbox" }));
  const size = screen.getByLabelText("Size") as HTMLSelectElement;
  const sleep = screen.getByLabelText("Auto sleep") as HTMLSelectElement;
  expect(size.disabled).toBe(true);
  expect(sleep.disabled).toBe(true);
  expect(size.selectedOptions[0].textContent).toBe("4 vCPU / 8 GB");
  expect(sleep.selectedOptions[0].textContent).toContain("30 min");
});

const paidSleep = [300, 600, 900, 1200, 1500, 1800].map((s) => ({ seconds: s, label: `After ${s / 60} min` }));

test("paid plan with always on: auto sleep defaults to 30 min, size to the first, Never reads plainly", async () => {
  show({ tier: "pro", sleepChoices: [...paidSleep, { seconds: 0, label: "Never" }] });
  await userEvent.click(await screen.findByRole("button", { name: "Create sandbox" }));
  const size = screen.getByLabelText("Size") as HTMLSelectElement;
  const sleep = screen.getByLabelText("Auto sleep") as HTMLSelectElement;
  expect(sleep.value).toBe("1800");
  expect(sleep.selectedOptions[0].textContent).toBe("After 30 min");
  expect([...sleep.options].map((o) => o.textContent)).toContain("Never");
  expect([...sleep.options].some((o) => /After Never|Off/.test(o.textContent ?? ""))).toBe(false);
  expect(size.selectedOptions[0].textContent).toBe("2 vCPU / 4 GB");
  await userEvent.type(screen.getByLabelText("Name"), "dev-2");
  await userEvent.click(screen.getByRole("button", { name: "Create & connect" }));
  expect(invoke).toHaveBeenCalledWith("create_sandbox", {
    name: "dev-2",
    snapshotId: undefined,
    vcpus: 2,
    memMiB: 4096,
    autoSuspendAfterSeconds: 1800,
  });
});

test("without 30 min the default is the first choice that sleeps", async () => {
  show({ tier: "pro", sleepChoices: [{ seconds: 0, label: "Never" }, ...paidSleep.slice(0, 2)] });
  await userEvent.click(await screen.findByRole("button", { name: "Create sandbox" }));
  expect((screen.getByLabelText("Auto sleep") as HTMLSelectElement).value).toBe("300");
});

test("My snapshot never creates a Standard sandbox and picks the first snapshot that arrives", async () => {
  show({ tier: "pro" });
  await userEvent.click(await screen.findByRole("button", { name: "Create sandbox" }));
  await userEvent.type(screen.getByLabelText("Name"), "dev-2");
  await userEvent.click(screen.getByRole("radio", { name: "My snapshot" }));
  const create = screen.getByRole("button", { name: "Create & connect" }) as HTMLButtonElement;
  expect(create.disabled).toBe(false);
  act(() => emit({ ...sample, tier: "pro", snapshots: [] }));
  expect(create.disabled).toBe(true);
  act(() => emit({ ...sample, tier: "pro", snapshots: [{ id: "s2", name: "snap-s2" }] }));
  expect((screen.getByLabelText("Snapshot") as HTMLSelectElement).value).toBe("s2");
  expect(create.disabled).toBe(false);
  await userEvent.click(create);
  expect(invoke).toHaveBeenCalledWith("create_sandbox", expect.objectContaining({ snapshotId: "s2" }));
});

test("resize starts at the current size, warns about the restart and applies another", async () => {
  show({ tier: "pro" }, [{ free: false, size: "4 vCPU / 8 GB" }]);
  await userEvent.click(await screen.findByRole("button", { name: "Details for dev-1" }));
  await userEvent.click(screen.getByRole("button", { name: "Resize" }));
  const select = screen.getByLabelText("New size") as HTMLSelectElement;
  expect(select.value).toBe("4 vCPU / 8 GB");
  const apply = screen.getAllByRole("button", { name: "Apply" })[0] as HTMLButtonElement;
  expect(apply.disabled).toBe(true);
  expect(screen.getByText("Restarts the sandbox")).toBeTruthy();
  await userEvent.selectOptions(select, "2 vCPU / 4 GB");
  expect(apply.disabled).toBe(false);
  await userEvent.click(apply);
  expect(invoke).toHaveBeenCalledWith("resize", { id: "a", vcpus: 2, memMiB: 4096 });
});


test("an unconnected sandbox offers Connect to T3 Code", async () => {
  show({}, [{ connected: false }]);
  expect(await screen.findByRole("button", { name: "Connect to T3 Code" })).toBeTruthy();
});

test("cooldown shows the cooling down line", async () => {
  show({ error: { kind: "cooldown", message: "x", minutes: 12 } }, [{}]);
  expect(await screen.findByText("Cooling down, ready in about 12 min")).toBeTruthy();
});

test("no T3 Code shows Download T3 Code", async () => {
  show({ t3: null });
  expect(await screen.findByText("Download T3 Code")).toBeTruthy();
});

test("free limit offers Connect on the unconnected sandbox, not create", async () => {
  show({ error: { kind: "freeLimit", message: "x" } }, [{ connected: false }]);
  expect(await screen.findByText("The free plan has one sandbox")).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Connect to T3 Code" }).length).toBe(2);
});

test("retry after a failed create connects the existing sandbox", async () => {
  show({ progress: { kind: "create", step: "installing", machineId: "m9", error: { kind: "setup", message: "tar: bad" } } }, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "+ New sandbox" }));
  expect(await screen.findByText("tar: bad")).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(invoke).toHaveBeenCalledWith("connect_sandbox", { id: "m9" });
  expect(invoke).not.toHaveBeenCalledWith("create_sandbox", expect.anything());
});

test("a failed connect shows one notice and one Retry, in list and new views", async () => {
  const err = { kind: "setup" as const, message: "tar: bad" };
  show({ error: err, progress: { kind: "create", step: "installing", machineId: "m9", error: err } }, [{}]);
  expect((await screen.findAllByText("tar: bad")).length).toBe(1);
  expect(screen.getAllByRole("button", { name: "Retry" }).length).toBe(1);
  await userEvent.click(screen.getByRole("button", { name: "+ New sandbox" }));
  expect(screen.getAllByText("tar: bad").length).toBe(1);
  expect(screen.getAllByRole("button", { name: "Retry" }).length).toBe(1);
});

test("a different state.error still shows next to a progress error", async () => {
  show(
    {
      error: { kind: "cooldown", message: "x", minutes: 12 },
      progress: { kind: "create", step: "installing", machineId: "m9", error: { kind: "setup", message: "tar: bad" } },
    },
    [{}],
  );
  expect((await screen.findAllByText("Cooling down, ready in about 12 min")).length).toBe(1);
  expect(screen.getAllByText("tar: bad").length).toBe(1);
  expect(screen.getAllByRole("button", { name: "Retry" }).length).toBe(1);
});

const tarBad = { kind: "setup" as const, message: "tar: bad" };

test("a successful create returns to the list and asks to confirm in T3 Code once", async () => {
  show({ tier: "pro" }, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "+ New sandbox" }));
  await userEvent.type(screen.getByLabelText("Name"), "dev-2");
  await userEvent.click(screen.getByRole("button", { name: "Create & connect" }));
  expect(await screen.findByText("Confirm the new environment in T3 Code")).toBeTruthy();
  expect(screen.queryByLabelText("Name")).toBeNull();
  expect(screen.getByText("dev-1")).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "Settings" }));
  expect(screen.queryByText("Confirm the new environment in T3 Code")).toBeNull();
});

test("a failed create keeps the form and its message, without the confirm line", async () => {
  show({ tier: "pro" }, [{}]);
  invoke.mockImplementation((cmd: string) =>
    cmd === "create_sandbox" ? Promise.reject({ kind: "api", message: "no_capacity" }) : Promise.resolve(),
  );
  await userEvent.click(await screen.findByRole("button", { name: "+ New sandbox" }));
  await userEvent.type(screen.getByLabelText("Name"), "dev-2");
  await userEvent.click(screen.getByRole("button", { name: "Create & connect" }));
  expect(await screen.findByText("no_capacity")).toBeTruthy();
  expect(screen.getByLabelText("Name")).toBeTruthy();
  expect(screen.queryByText("Confirm the new environment in T3 Code")).toBeNull();
});

test("a failed create with a sandbox offers only Retry, not Create", async () => {
  show({ tier: "pro", progress: { kind: "create", step: "installing", machineId: "m9", error: tarBad } }, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "+ New sandbox" }));
  await userEvent.type(screen.getByLabelText("Name"), "dev-2");
  expect((screen.getByRole("button", { name: "Create & connect" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getAllByRole("button", { name: "Retry" }).length).toBe(1);
});

test("× closes a failed progress", async () => {
  show({ progress: { kind: "create", step: "installing", machineId: "m9", error: tarBad } }, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "Dismiss" }));
  expect(invoke).toHaveBeenCalledWith("dismiss_progress");
});

test("a rejected connect that is the progress error shows once", async () => {
  show({ error: tarBad, progress: { kind: "connect", step: "installing", machineId: "a", error: tarBad } }, [{ connected: false }]);
  invoke.mockImplementation((cmd: string) => (cmd === "connect_sandbox" ? Promise.reject(tarBad) : Promise.resolve()));
  await userEvent.click(await screen.findByRole("button", { name: "Retry" }));
  await waitFor(() => expect(invoke).toHaveBeenCalledWith("connect_sandbox", { id: "a" }));
  expect(screen.getAllByText("tar: bad").length).toBe(1);
  expect(screen.getAllByRole("button", { name: "Retry" }).length).toBe(1);
});

test("another setup error (a reinstall) gets no Retry of the progress sandbox", async () => {
  show({ error: { kind: "setup", message: "reinstall: boom" }, progress: { kind: "create", step: "installing", machineId: "m9", error: tarBad } }, [
    {},
  ]);
  expect(await screen.findByText("reinstall: boom")).toBeTruthy();
  // The one Retry is the progress's own.
  expect(screen.getAllByRole("button", { name: "Retry" }).length).toBe(1);
  expect(screen.getByText("tar: bad").closest(".progress")).toBeTruthy();
});

test("connecting an existing sandbox starts at Wake, not Create", async () => {
  show({ progress: { kind: "connect", step: "connecting", machineId: "a", error: null } }, [{}]);
  expect(await screen.findByText("Wake sandbox")).toBeTruthy();
  expect(screen.queryByText("Create sandbox")).toBeNull();
});

test("details of an unconnected sandbox offer Connect to T3 Code", async () => {
  show({}, [{ connected: false }]);
  await userEvent.click(await screen.findByRole("button", { name: "Details for dev-1" }));
  expect(screen.queryByRole("button", { name: "Open in T3 Code" })).toBeNull();
  await userEvent.click(screen.getByRole("button", { name: "Connect to T3 Code" }));
  expect(invoke).toHaveBeenCalledWith("connect_sandbox", { id: "a" });
  expect(await screen.findByText("Confirm the new environment in T3 Code")).toBeTruthy();
});

test("details ask for the SSH command only when the row is opened", async () => {
  show({}, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "Details for dev-1" }));
  await screen.findByText("Can run 1 h 20 m more");
  expect(invoke).not.toHaveBeenCalledWith("ssh_command", expect.anything());
  await userEvent.click(screen.getByRole("button", { name: "Show command" }));
  expect(await screen.findByText("ssh dev-1@ssh.instabox.sh")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Copy" })).toBeTruthy();
});

test("a failed SSH command shows as an error, without Copy", async () => {
  show({}, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "Details for dev-1" }));
  invoke.mockImplementation((cmd: string) =>
    cmd === "ssh_command" ? Promise.reject({ kind: "api", message: "machine_not_running" }) : Promise.resolve(false),
  );
  await userEvent.click(screen.getByRole("button", { name: "Show command" }));
  const err = await screen.findByText("machine_not_running");
  expect(err.getAttribute("role")).toBe("alert");
  expect(screen.queryByRole("button", { name: "Copy" })).toBeNull();
});

test("first run without T3 Code: Check again looks for it now", async () => {
  show({ t3: null });
  await userEvent.click(await screen.findByRole("button", { name: "Check again" }));
  expect(invoke).toHaveBeenCalledWith("refresh_t3");
});

test("desktopBehind never offers Update", async () => {
  show({}, [{ desktopBehind: true, updatePending: true }]);
  expect(await screen.findByText(/This sandbox runs a newer T3/)).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Update" })).toBeNull();
});

test("updatePending offers Update that reinstalls", async () => {
  show({}, [{ updatePending: true, t3Version: "0.1.2" }]);
  expect(await screen.findByText(/T3 0.1.2 · T3 Code 0.1.10/)).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "Update" }));
  expect(invoke).toHaveBeenCalledWith("reinstall_t3", { id: "a" });
});

test("details checks agent status once for a running sandbox", async () => {
  show({}, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "Details for dev-1" }));
  await screen.findByText("Can run 1 h 20 m more");
  await waitFor(() => expect(invoke.mock.calls.filter((c) => c[0] === "agent_status").length).toBe(2));
});

test("sign-in screen shows the device code", async () => {
  show({ signedIn: false, signInCode: "ABCD-1234" });
  expect(await screen.findByText("ABCD-1234")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Sign in with GitHub" })).toBeTruthy();
});

test("settings: MCP buttons follow the CLI status", async () => {
  show({ mcp: { claudeCode: null, codex: false } }, [{}]);
  await userEvent.click(await screen.findByRole("button", { name: "Settings" }));
  expect(screen.getByText("Not installed")).toBeTruthy();
  await userEvent.click(screen.getAllByRole("button", { name: "Add" })[0]);
  expect(invoke).toHaveBeenCalledWith("add_mcp", { client: "codex" });
});

test("a pushed state replaces the screen", async () => {
  show({}, [{}]);
  await screen.findByText("dev-1");
  emit({ ...sample, sandboxes: [{ ...box, name: "other" }] });
  expect(await screen.findByText("other")).toBeTruthy();
});
