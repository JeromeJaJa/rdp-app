// What changed in a release, from its commits: everything since the previous release tag,
// grouped by conventional-commit type, each linked to its commit.
//
//   bun scripts/release-notes.mjs [<to-ref>]           prints the notes (to HEAD by default)
//   bun scripts/release-notes.mjs --changelog <ver>    adds a "## [<ver>]" section to CHANGELOG.md
//
// The release workflow prints them as the GitHub release's notes; making a release runs
// --changelog first, so CHANGELOG.md keeps the same list.
import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";

const git = (...args) => execFileSync("git", args, { encoding: "utf8" }).trim();

const args = process.argv.slice(2);
const changelogVersion = args[0] === "--changelog" ? args[1] : null;
const to = changelogVersion ? "HEAD" : (args[0] ?? "HEAD");

/** The release tag before `ref` (the newest v* tag that's an ancestor of `ref`'s parent). */
function previousTag(ref) {
  try {
    return git("describe", "--tags", "--abbrev=0", "--match", "v*", `${ref}^`);
  } catch {
    return null; // the first release
  }
}

/** https://github.com/owner/repo, from the CI environment or the origin remote. */
function repoUrl() {
  if (process.env.GITHUB_REPOSITORY) {
    return `${process.env.GITHUB_SERVER_URL ?? "https://github.com"}/${process.env.GITHUB_REPOSITORY}`;
  }
  try {
    const origin = git("remote", "get-url", "origin");
    const m = origin.match(/github\.com[:/](.+?)(?:\.git)?$/);
    return m ? `https://github.com/${m[1]}` : null;
  } catch {
    return null;
  }
}

const GROUPS = [
  ["Features", ["feat"]],
  ["Fixes", ["fix"]],
  ["Performance", ["perf"]],
  ["Other changes", ["refactor", "docs", "test", "ci", "build", "chore", "style", "deps"]],
];

const from = previousTag(to);
const range = from ? `${from}..${to}` : to;
const repo = repoUrl();
const SEP = "\x1f";
const commits = git("log", "--no-merges", "--reverse", `--format=%h${SEP}%s`, range)
  .split("\n")
  .filter(Boolean)
  .map((line) => {
    const [hash, subject] = line.split(SEP);
    const m = subject.match(/^(\w+)(?:\(([^)]+)\))?(!)?:\s*(.+)$/);
    return m
      ? { hash, type: m[1], scope: m[2], breaking: !!m[3], text: m[4] }
      : { hash, type: "other", scope: null, breaking: false, text: subject };
  })
  // The version bump itself isn't a change worth listing.
  .filter((c) => !(c.type === "chore" && c.scope === "release"));

const line = (c) => {
  const ref = repo ? `[\`${c.hash}\`](${repo}/commit/${c.hash})` : `\`${c.hash}\``;
  const scope = c.scope ? `**${c.scope}:** ` : "";
  return `- ${c.breaking ? "**Breaking:** " : ""}${scope}${c.text} ${ref}`;
};

const sections = [];
for (const [title, types] of GROUPS) {
  const these = commits.filter((c) => types.includes(c.type) || (title === "Other changes" && c.type === "other"));
  if (these.length) sections.push(`### ${title}\n\n${these.map(line).join("\n")}`);
}
const unknown = commits.filter((c) => c.type !== "other" && !GROUPS.some(([, t]) => t.includes(c.type)));
if (unknown.length) sections.push(`### More\n\n${unknown.map(line).join("\n")}`);
let notes = sections.join("\n\n") || "No changes.";
const toLabel = changelogVersion ? `v${changelogVersion}` : to;
if (repo && from) notes += `\n\n**All changes**: ${repo}/compare/${from}...${toLabel}`;

if (!changelogVersion) {
  console.log(notes);
} else {
  // Replace an "Unreleased" section (its entries are among these commits) with this release's.
  const path = new URL("../CHANGELOG.md", import.meta.url);
  let log = readFileSync(path, "utf8");
  log = log.replace(/^## \[Unreleased\][\s\S]*?(?=^## \[)/m, "");
  const date = new Date().toISOString().slice(0, 10);
  const section = `## [${changelogVersion}] - ${date}\n\n${notes.replace(/\n\n\*\*All changes\*\*:.*$/, "")}\n\n`;
  const first = log.search(/^## \[/m);
  log = first < 0 ? `${log.trimEnd()}\n\n${section}` : log.slice(0, first) + section + log.slice(first);
  if (repo && from) {
    const link = `[${changelogVersion}]: ${repo}/compare/${from}...v${changelogVersion}\n`;
    const firstLink = log.search(/^\[\d/m);
    log = firstLink < 0 ? `${log.trimEnd()}\n\n${link}` : log.slice(0, firstLink) + link + log.slice(firstLink);
  }
  writeFileSync(path, log);
  console.log(`CHANGELOG.md: added ${changelogVersion} (${commits.length} commits since ${from ?? "the start"})`);
}
