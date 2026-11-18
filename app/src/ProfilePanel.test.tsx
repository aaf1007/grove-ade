import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { fakeBridge } from "./fakeBridge";
import type { MonacoEditorProps } from "./MonacoEditor";
import { ProfilePanel } from "./ProfilePanel";

// Monaco needs a real browser. The stub shows the text the panel hands it, takes typing, and
// shows the line it marks as a problem.
vi.mock("./MonacoEditor", () => ({
  MonacoEditor: ({ path, text, onChange, problemLine }: MonacoEditorProps) => (
    <div>
      <textarea aria-label={`Editor for ${path}`} value={text} onChange={(e) => onChange(e.target.value)} />
      {problemLine != null && <p aria-label={`Marked line in ${path}`}>{problemLine}</p>}
    </div>
  ),
}));

// What the panel asks the window to start, as the window's Start handler.
const onStartRun = vi.fn();

afterEach(() => {
  cleanup();
  onStartRun.mockReset();
});

const main = "/code/grove";
const feature = "/code/grove-wt/feat-57";
const RUST = `name = "Rust crate"\nsuggest_for = ["Cargo.toml"]\nsetup = "cargo fetch"\n`;
const NEXT = `name = "Next.js app"\nsuggest_for = ["next.config.js"]\n`;

async function setup(profile: string | null = "Rust crate") {
  const bridge = fakeBridge(
    {
      [main]: [
        { path: main, branch: "main", isMain: true },
        { path: feature, branch: "feat-57", isMain: false },
      ],
    },
    {},
    {},
    [
      { name: "Next.js app", suggestFor: ["next.config.js"], text: NEXT },
      { name: "Rust crate", suggestFor: ["Cargo.toml"], text: RUST },
    ],
  );
  await bridge.addRepo(main);
  await bridge.setRepoProfile(main, profile);
  render(<ProfilePanel bridge={bridge} worktree={feature} onStartRun={onStartRun} />);
  await settle();
  return bridge;
}

// Lets the bridge's promises settle.
const settle = () => act(async () => {});

const profileEditor = () => screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Editor for profile.toml" });
const overridesEditor = () => screen.getByRole<HTMLTextAreaElement>("textbox", { name: "Editor for overrides.toml" });
const picker = () => screen.getByLabelText<HTMLSelectElement>("Profile");
const picked = () => picker().selectedOptions[0].textContent;
const section = (name: string) => screen.getByRole("region", { name });

function type(editor: HTMLTextAreaElement, text: string) {
  fireEvent.change(editor, { target: { value: text } });
}

async function click(name: string, within_: HTMLElement = document.body) {
  fireEvent.click(within(within_).getByRole("button", { name }));
  await settle();
}

async function pick(profile: string) {
  const option = [...picker().options].find((o) => o.textContent === profile)!;
  fireEvent.change(picker(), { target: { value: option.value } });
  await settle();
}

test("the panel shows the repo's profile file, where it lives and who uses it, then the repo's overrides", async () => {
  await setup();

  expect(profileEditor().value).toBe(RUST);
  expect(screen.getByText("/home/.grovecode/profiles/rust-crate.toml")).toBeTruthy();
  expect(screen.getByText("Used by grove")).toBeTruthy();
  expect(picked()).toBe("Rust crate");
  expect([...picker().options].map((o) => o.textContent)).toEqual(["Next.js app", "Rust crate", "None"]);
  expect(overridesEditor().value).toBe("");
});

test("saving the profile sends its text to the core under its name, with the button or Cmd-S", async () => {
  const bridge = await setup();
  const edited = RUST.replace("cargo fetch", "cargo build");

  type(profileEditor(), edited);
  expect(within(section("Profile file")).getByText("Unsaved")).toBeTruthy();
  await click("Save", section("Profile file"));

  expect(bridge.profileTextsSet).toEqual([{ name: "Rust crate", text: edited }]);
  expect(within(section("Profile file")).queryByText("Unsaved")).toBeNull();
  expect((await bridge.profileText("Rust crate")).text).toBe(edited);

  type(profileEditor(), `# Mine now\n${edited}`);
  fireEvent.keyDown(profileEditor(), { key: "s", metaKey: true });
  await settle();

  expect(bridge.profileTextsSet.at(-1)).toEqual({ name: "Rust crate", text: `# Mine now\n${edited}` });
});

test("saving the overrides sends them to the core for the repo and leaves the profile as it was", async () => {
  const bridge = await setup();
  const overrides = `[agent]\nmodel = "haiku"\n`;

  type(overridesEditor(), overrides);
  await click("Save", section("Overrides"));

  expect(bridge.overridesSet).toEqual([{ repo: main, text: overrides }]);
  expect(bridge.profileTextsSet).toEqual([]);
  expect(await bridge.repoProfile(main)).toBe("Rust crate");
  expect(await bridge.repoOverridesText(main)).toBe(overrides);
});

test("after a save the overrides show what grove kept, which has no comments", async () => {
  await setup();

  type(overridesEditor(), `# Faster here\nsetup = "make"\n`);
  await click("Save", section("Overrides"));

  expect(overridesEditor().value).toBe(`setup = "make"\n`);
  expect(within(section("Overrides")).queryByText("Unsaved")).toBeNull();
});

test("an invalid profile shows the core's error at its line, keeps the text and saves nothing", async () => {
  const bridge = await setup();
  const broken = `name = "Rust crate"\n[agent]\nmodel "opus"\n`;

  type(profileEditor(), broken);
  await click("Save", section("Profile file"));

  expect(within(section("Profile file")).getByRole("alert").textContent).toBe("Line 3: expected `=`");
  expect(screen.getByLabelText("Marked line in profile.toml").textContent).toBe("3");
  expect(profileEditor().value).toBe(broken);
  expect(bridge.profileTextsSet).toEqual([]);
  expect((await bridge.profileText("Rust crate")).text).toBe(RUST);

  type(profileEditor(), `name = "Rust crate"\n[agent]\nmodel = "opus"\n`);
  await click("Save", section("Profile file"));

  expect(within(section("Profile file")).queryByRole("alert")).toBeNull();
  expect(screen.queryByLabelText("Marked line in profile.toml")).toBeNull();
  expect(bridge.profileTextsSet).toHaveLength(1);
});

test("renaming the profile is refused at the name's line", async () => {
  const bridge = await setup();

  type(profileEditor(), RUST.replace("Rust crate", "Next.js app"));
  await click("Save", section("Profile file"));

  expect(within(section("Profile file")).getByRole("alert").textContent).toBe(
    'Line 1: name: it must stay "Rust crate": renaming a profile is not supported',
  );
  expect((await bridge.profileText("Next.js app")).text).toBe(NEXT);
});

test("invalid overrides show the core's error at its line and save nothing", async () => {
  const bridge = await setup();

  type(overridesEditor(), `setup = "make"\noops\n`);
  await click("Save", section("Overrides"));

  expect(within(section("Overrides")).getByRole("alert").textContent).toBe("Line 2: expected `=`");
  expect(screen.getByLabelText("Marked line in overrides.toml").textContent).toBe("2");
  expect(bridge.overridesSet).toEqual([]);
});

test("picking another profile makes the repo use it and shows its file, and None shows none", async () => {
  const bridge = await setup();

  await pick("Next.js app");

  expect(await bridge.repoProfile(main)).toBe("Next.js app");
  expect(profileEditor().value).toBe(NEXT);
  expect(screen.getByText("/home/.grovecode/profiles/next-js-app.toml")).toBeTruthy();

  await pick("None");

  expect(await bridge.repoProfile(main)).toBeNull();
  expect(screen.queryByRole("textbox", { name: "Editor for profile.toml" })).toBeNull();
  expect(screen.getByText("grove uses no profile for this repo. Pick one to see and edit it.")).toBeTruthy();
  expect(overridesEditor()).toBeTruthy();
});

test("a repo with no profile yet can pick one here", async () => {
  const bridge = await setup(null);

  expect(picked()).toBe("None");
  await pick("Rust crate");

  expect(await bridge.repoProfile(main)).toBe("Rust crate");
  expect(profileEditor().value).toBe(RUST);
});

test("the picker waits while the profile has unsaved edits, so a pick never drops them", async () => {
  await setup();

  type(profileEditor(), `${RUST}# more\n`);

  expect(picker().disabled).toBe(true);
  expect(picker().title).toBe("Save or discard your edits to the profile first.");

  await click("Discard", section("Profile file"));

  expect(profileEditor().value).toBe(RUST);
  expect(picker().disabled).toBe(false);
});

test("when the repo switches profile elsewhere, unsaved edits stay until saved or discarded, with a notice", async () => {
  const bridge = await setup();
  const mine = `${RUST}# mine\n`;

  type(profileEditor(), mine);
  await bridge.setRepoProfile(main, "Next.js app");
  act(() => bridge.changeProfileOnDisk("Next.js app", NEXT));
  await settle();

  expect(profileEditor().value).toBe(mine);
  expect(picked()).toBe("Rust crate");
  expect(within(section("Profile file")).getByRole("alert").textContent).toBe(
    "This repo now uses Next.js app. Save or discard your edits to Rust crate to switch to it.",
  );

  await click("Save", section("Profile file"));

  expect(bridge.profileTextsSet).toEqual([{ name: "Rust crate", text: mine }]);
  expect(picked()).toBe("Next.js app");
  expect(profileEditor().value).toBe(NEXT);
  expect(within(section("Profile file")).queryByRole("alert")).toBeNull();
});

test("when the repo drops its profile elsewhere, discarding the edits shows no profile", async () => {
  const bridge = await setup();

  type(profileEditor(), `${RUST}# mine\n`);
  await bridge.setRepoProfile(main, null);
  act(() => bridge.changeProfileOnDisk("Next.js app", NEXT));
  await settle();

  expect(within(section("Profile file")).getByRole("alert").textContent).toBe(
    "This repo now uses no profile. Save or discard your edits to Rust crate to switch to it.",
  );
  await click("Discard", section("Profile file"));

  expect(picked()).toBe("None");
  expect(screen.queryByRole("textbox", { name: "Editor for profile.toml" })).toBeNull();
});

test("a profile file that is not valid shows its error, and the repo's profile and overrides still show", async () => {
  const bridge = fakeBridge({ [main]: [{ path: main, branch: "main", isMain: true }] });
  await bridge.addRepo(main);
  await bridge.setRepoProfile(main, "Rust crate");
  const broken = "the profile /home/.grovecode/profiles/go.toml is not valid at line 3: expected `=`";
  bridge.profileNames = async () => {
    throw broken;
  };

  render(<ProfilePanel bridge={bridge} worktree={main} onStartRun={onStartRun} />);
  await settle();

  expect(screen.getByRole("alert").textContent).toBe(broken);
  expect(picked()).toBe("Rust crate");
  expect(profileEditor().value).toBe(RUST);
  expect(overridesEditor().value).toBe("");
});

test("a change on disk shows in the panel without a reload", async () => {
  const bridge = await setup();
  const changed = RUST.replace("cargo fetch", "cargo fetch --locked");

  act(() => bridge.changeProfileOnDisk("Rust crate", changed));
  await settle();

  expect(profileEditor().value).toBe(changed);
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a change on disk keeps unsaved edits, says so, and lets the user pick which to keep", async () => {
  const bridge = await setup();
  const mine = `${RUST}# mine\n`;
  const theirs = `${RUST}# theirs\n`;

  type(profileEditor(), mine);
  act(() => bridge.changeProfileOnDisk("Rust crate", theirs));
  await settle();

  expect(profileEditor().value).toBe(mine);
  const notice = within(section("Profile file")).getByRole("alert");
  expect(notice.textContent).toContain("The profile changed on disk. Your edits are kept.");

  await click("Load the disk version", notice);

  expect(profileEditor().value).toBe(theirs);
  expect(within(section("Profile file")).queryByRole("alert")).toBeNull();

  type(profileEditor(), mine);
  act(() => bridge.changeProfileOnDisk("Rust crate", RUST));
  await settle();
  await click("Keep my edits", within(section("Profile file")).getByRole("alert"));

  expect(within(section("Profile file")).queryByRole("alert")).toBeNull();
  expect(profileEditor().value).toBe(mine);
  await click("Save", section("Profile file"));
  expect(bridge.profileTextsSet.at(-1)).toEqual({ name: "Rust crate", text: mine });
});

test("a change on disk right after the panel's save keeps the edits typed since, with a notice", async () => {
  const bridge = await setup();
  const saved = `${RUST}# one\n`;
  const theirs = `${RUST}# theirs\n`;
  const store = bridge.setProfileText;
  bridge.setProfileText = async (name, text) => {
    await store(name, text);
    bridge.changeProfileOnDisk(name, theirs);
  };

  type(profileEditor(), saved);
  fireEvent.click(within(section("Profile file")).getByRole("button", { name: "Save" }));
  type(profileEditor(), `${saved}# two\n`);
  await settle();

  expect(profileEditor().value).toBe(`${saved}# two\n`);
  expect(within(section("Profile file")).getByRole("alert").textContent).toContain(
    "The profile changed on disk. Your edits are kept.",
  );
  await click("Load the disk version", section("Profile file"));
  expect(profileEditor().value).toBe(theirs);
});

test("the panel's own save is not a change on disk, even with typing after it", async () => {
  await setup();
  const saved = `${RUST}# one\n`;

  type(profileEditor(), saved);
  fireEvent.click(within(section("Profile file")).getByRole("button", { name: "Save" }));
  type(profileEditor(), `${saved}# two\n`);
  await settle();

  expect(within(section("Profile file")).queryByRole("alert")).toBeNull();
  expect(profileEditor().value).toBe(`${saved}# two\n`);
  expect(within(section("Profile file")).getByText("Unsaved")).toBeTruthy();
});

const TESTS_RUN = `\n[[run_session]]\nname = "tests"\ncommand = "cargo test"\n`;
const runSessions = () => within(section("Run sessions"));
const runNames = () => runSessions().queryAllByRole("listitem").map((item) => item.textContent);

test("each saved run session of the repo shows with its command, and Start asks the window to start it", async () => {
  const bridge = await setup();
  bridge.changeProfileOnDisk("Rust crate", RUST + TESTS_RUN);
  await settle();

  expect(runNames()).toEqual(["tests cargo test Start"]);

  await click("Start tests", section("Run sessions"));

  expect(onStartRun).toHaveBeenCalledExactlyOnceWith("tests");
});

test("a repo with no run sessions says where to add one", async () => {
  await setup();

  expect(runNames()).toEqual([]);
  expect(runSessions().getByText(/No run sessions/)).toBeTruthy();
});

test("the run sessions follow the overrides once they are saved", async () => {
  const bridge = await setup();
  bridge.changeProfileOnDisk("Rust crate", RUST + TESTS_RUN);
  await settle();

  type(overridesEditor(), `[[run_session]]\nname = "dev"\ncommand = "pnpm dev"\nport = 5173\n`);
  expect(runNames()).toEqual(["tests cargo test Start"]);
  await click("Save", section("Overrides"));

  expect(runNames()).toEqual(["dev pnpm dev Start"]);
});


test("Start waits while the run sessions are listed again after a change", async () => {
  const bridge = await setup();
  bridge.changeProfileOnDisk("Rust crate", RUST + TESTS_RUN);
  await settle();
  const runSessionsOf = bridge.runSessions.bind(bridge);
  let reply = async () => {};
  vi.spyOn(bridge, "runSessions").mockImplementation(
    (repo) => new Promise((resolve) => (reply = async () => resolve(await runSessionsOf(repo)))),
  );

  bridge.changeProfileOnDisk("Rust crate", RUST + TESTS_RUN.replace("cargo test", "cargo nextest run"));
  await settle();

  expect(runSessions().getByRole<HTMLButtonElement>("button", { name: "Start tests" }).disabled).toBe(true);

  await act(async () => reply());

  expect(runNames()).toEqual(["tests cargo nextest run Start"]);
  expect(runSessions().getByRole<HTMLButtonElement>("button", { name: "Start tests" }).disabled).toBe(false);
});
test("the run sessions follow the profile the repo picks", async () => {
  const bridge = await setup();
  bridge.changeProfileOnDisk("Next.js app", NEXT + `\n[[run_session]]\nname = "dev"\ncommand = "next dev"\n`);
  await settle();
  expect(runNames()).toEqual([]);

  await pick("Next.js app");

  expect(runNames()).toEqual(["dev next dev Start"]);
});

// Sync (#174): every Profile panel of a repo follows a change to its settings made elsewhere.

test("a profile picked for the repo elsewhere, as in the New worktree dialog, shows in the panel", async () => {
  const bridge = await setup();

  await act(() => bridge.setRepoProfile(main, "Next.js app"));
  await settle();

  expect(picked()).toBe("Next.js app");
  expect(profileEditor().value).toBe(NEXT);
});

test("overrides saved for the repo in another panel show in this one", async () => {
  const bridge = await setup();
  const theirs = `setup = "make"\n`;

  await act(() => bridge.setRepoOverridesText(main, theirs));
  await settle();

  expect(overridesEditor().value).toBe(theirs);
  expect(within(section("Overrides")).queryByRole("alert")).toBeNull();
});

test("overrides saved in another panel keep this panel's unsaved edits, with a notice", async () => {
  const bridge = await setup();
  const mine = `setup = "mine"\n`;
  const theirs = `setup = "theirs"\n`;

  type(overridesEditor(), mine);
  await act(() => bridge.setRepoOverridesText(main, theirs));
  await settle();

  expect(overridesEditor().value).toBe(mine);
  const notice = within(section("Overrides")).getByRole("alert");
  expect(notice.textContent).toContain("The overrides changed. Your edits are kept.");
  await click("Load the disk version", notice);
  expect(overridesEditor().value).toBe(theirs);
});

test("a change to another repo's settings leaves the panel alone", async () => {
  const other = "/code/other";
  const bridge = fakeBridge(
    {
      [main]: [{ path: main, branch: "main", isMain: true }],
      [other]: [{ path: other, branch: "main", isMain: true }],
    },
    {},
    {},
    [{ name: "Rust crate", suggestFor: ["Cargo.toml"], text: RUST }],
  );
  await bridge.addRepo(main);
  await bridge.addRepo(other);
  await bridge.setRepoProfile(main, "Rust crate");
  const read = bridge.repoOverridesText;
  const reads: string[] = [];
  bridge.repoOverridesText = (repo) => {
    reads.push(repo);
    return read(repo);
  };
  render(<ProfilePanel bridge={bridge} worktree={main} onStartRun={onStartRun} />);
  await settle();
  reads.length = 0;

  await act(() => bridge.setRepoOverridesText(other, `setup = "make"\n`));
  await act(() => bridge.setRepoProfile(other, null));
  await settle();

  expect(reads).toEqual([]);
  expect(overridesEditor().value).toBe("");
  expect(picked()).toBe("Rust crate");
});

// The event can reach the panel before the save's own reply does, as it can over Tauri.
test("the panel's own overrides save comes back as grove kept it, once, with no notice", async () => {
  const bridge = await setup();
  const store = bridge.setRepoOverridesText;
  let reply = () => {};
  bridge.setRepoOverridesText = async (repo, text) => {
    await store(repo, text);
    await new Promise<void>((resolve) => (reply = resolve));
  };

  type(overridesEditor(), `# mine\nsetup = "make"\n`);
  await click("Save", section("Overrides"));
  expect(within(section("Overrides")).queryByRole("alert")).toBeNull();
  await act(async () => reply());

  expect(bridge.overridesSet).toEqual([{ repo: main, text: `# mine\nsetup = "make"\n` }]);
  expect(overridesEditor().value).toBe(`setup = "make"\n`);
  expect(within(section("Overrides")).queryByRole("alert")).toBeNull();
  expect(within(section("Overrides")).queryByText("Unsaved")).toBeNull();
});

// A second save while the first is in flight, with the first save's event and the first save's
// own reload both arriving late, as they can over Tauri.
test("two saves in a row show no notice for the panel's own saves", async () => {
  const bridge = fakeBridge(
    { [main]: [{ path: main, branch: "main", isMain: true }] },
    {},
    {},
    [{ name: "Rust crate", suggestFor: ["Cargo.toml"], text: RUST }],
  );
  await bridge.addRepo(main);
  await bridge.setRepoProfile(main, "Rust crate");
  // The test delivers the repo's events itself.
  let deliver: (repo: string) => void = () => {};
  bridge.onRepoSettingsChanged = (listener) => {
    deliver = listener;
    return () => {};
  };
  // Each read takes the next hold, if any, after it has read.
  const holds: (Promise<void> | undefined)[] = [];
  const read = bridge.repoOverridesText;
  bridge.repoOverridesText = async (repo) => {
    const text = await read(repo);
    await holds.shift();
    return text;
  };
  render(<ProfilePanel bridge={bridge} worktree={main} onStartRun={onStartRun} />);
  await settle();
  let release1 = () => {};
  let release2 = () => {};
  const a = `# a\nsetup = "a"\n`;
  const b = `# b\nsetup = "b"\n`;

  holds.push(new Promise((resolve) => (release1 = resolve)));
  type(overridesEditor(), a);
  await click("Save", section("Overrides"));
  act(() => deliver(main));
  await settle();
  holds.push(new Promise((resolve) => (release2 = resolve)));
  type(overridesEditor(), b);
  await click("Save", section("Overrides"));
  await act(async () => release1());
  await settle();

  expect(within(section("Overrides")).queryByRole("alert")).toBeNull();

  await act(async () => release2());
  await settle();

  expect(within(section("Overrides")).queryByRole("alert")).toBeNull();
  expect(bridge.overridesSet.map((o) => o.text)).toEqual([a, b]);
  expect(overridesEditor().value).toBe(`setup = "b"\n`);
  expect(within(section("Overrides")).queryByText("Unsaved")).toBeNull();
});

// The first save's reply comes back after the second save was made.
test("a save made while another is in flight is stored last and stays saved", async () => {
  const bridge = await setup();
  const store = bridge.setProfileText;
  let replyFirst = () => {};
  let calls = 0;
  bridge.setProfileText = async (name, text) => {
    await store(name, text);
    if (++calls === 1) await new Promise<void>((resolve) => (replyFirst = resolve));
  };
  const first = `${RUST}# one\n`;
  const second = `${RUST}# two\n`;

  type(profileEditor(), first);
  await click("Save", section("Profile file"));
  type(profileEditor(), second);
  await click("Save", section("Profile file"));
  await act(async () => replyFirst());
  await settle();

  expect(bridge.profileTextsSet.map((t) => t.text)).toEqual([first, second]);
  expect((await bridge.profileText("Rust crate")).text).toBe(second);
  expect(profileEditor().value).toBe(second);
  expect(within(section("Profile file")).queryByText("Unsaved")).toBeNull();
  expect(within(section("Profile file")).queryByRole("alert")).toBeNull();
});

// Another panel saves after this panel's first save came back, before its queued second save.
// A change made during the first save is not caught: that needs the core to refuse a stale save.
test("a queued save does not write over a save made elsewhere while it waited", async () => {
  const bridge = await setup();
  const store = bridge.setRepoOverridesText;
  let replyFirst = () => {};
  let calls = 0;
  bridge.setRepoOverridesText = async (repo, text) => {
    await store(repo, text);
    if (++calls === 1) await new Promise<void>((resolve) => (replyFirst = resolve));
  };
  const a = `setup = "a"\n`;
  const b = `setup = "b"\n`;
  const theirs = `setup = "theirs"\n`;
  // The first save checks, stores and reads back. The other panel saves just before the second
  // save checks.
  const read = bridge.repoOverridesText;
  let reads = 0;
  bridge.repoOverridesText = async (repo) => {
    if (++reads === 3) await store(main, theirs);
    return read(repo);
  };

  type(overridesEditor(), a);
  await click("Save", section("Overrides"));
  type(overridesEditor(), b);
  await click("Save", section("Overrides"));
  await act(async () => replyFirst());
  await settle();

  expect(bridge.overridesSet.map((o) => o.text)).toEqual([a, theirs]);
  expect(await read(main)).toBe(theirs);
  expect(overridesEditor().value).toBe(b);
  const notice = within(section("Overrides")).getByRole("alert");
  expect(notice.textContent).toContain("The overrides changed. Your edits are kept.");
  await click("Load the disk version", notice);
  expect(overridesEditor().value).toBe(theirs);
});

// Repair (#174): a profile file broken outside grove can be fixed in the panel.

test("a profile file on disk with no name shows the error, as the core does", async () => {
  const bridge = await setup();

  act(() => bridge.changeProfileOnDisk("Rust crate", `setup = "cargo fetch"\n`));
  await settle();

  expect(screen.getByRole("alert").textContent).toContain("rust-crate.toml is not valid at line 1: missing field `name`");
});

test("a profile file broken on disk shows its text and the error, and a valid save repairs it", async () => {
  const bridge = await setup();
  const broken = `${RUST}[agent\n`;

  act(() => bridge.changeProfileOnDisk("Rust crate", broken));
  await settle();

  expect(profileEditor().value).toBe(broken);
  expect(screen.getByRole("alert").textContent).toContain("rust-crate.toml is not valid at line 4");

  const fixed = `${RUST}[agent]\n`;
  type(profileEditor(), fixed);
  await click("Save", section("Profile file"));

  expect(bridge.profileTextsSet).toEqual([{ name: "Rust crate", text: fixed }]);
  expect(profileEditor().value).toBe(fixed);
  expect(screen.queryByRole("alert")).toBeNull();
});
