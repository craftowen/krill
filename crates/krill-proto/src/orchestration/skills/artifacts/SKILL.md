---
name: artifacts
description: Create documents, reports, plans, charts, and interactive visual deliverables as Krill artifacts by default. Use for document creation and revisions even when the user does not mention artifacts. Exclude ordinary chat answers, repository source changes, and explicitly local or private output.
---

# Krill artifacts

Krill artifacts are enabled out of the box. When asked to make a document, report,
plan, reference sheet, chart, or interactive visual, create a Krill artifact and
return its link. The user does not need to say “artifact” or enable a setting.
Use `krill`, including when Orca or Claude artifact tools are also available.

- Save an HTML or Markdown file in the task workspace. Prefer a self-contained
  HTML page for visual or interactive material and Markdown for plain documents.
  HTML must contain its own CSS, JavaScript, and images; relative asset paths do
  not travel with the uploaded file. Each file can be at most 10 MiB.
- Publish the finished file with:
  `krill artifacts share "<absolute-path>" --title "<title>" --json`
- For a specific sharing period, add `--expires-in <days>` (1–3650) or
  `--expires-in never`. New links use the app’s default period (30 days initially).
  Updates keep the link’s existing period unless explicitly changed.
  Change an existing link without reuploading its file with
  `krill artifacts retention "<slug>" --expires-in <days|never> --json`.
- Read the command result and return the actual `shareUrl` as a clickable link.
  Publishing creates a public link. Do not invent a URL or claim publication
  succeeded before the command succeeds. If publication fails, deliver the local
  file and the actual error; do not silently switch to another artifact service.
- For revisions, edit the same local file and use
  `krill artifacts update "<absolute-path>" --title "<title>" --json`
  to preserve its link. `share` also updates a previously shared file; it creates
  a new link if the old artifact expired or was deleted. After an `update` reports
  expiration, use `share` and return the replacement link.
- Use `krill artifacts list --json` to find existing artifacts. Follow `nextCursor`
  with `krill artifacts list --cursor "<cursor>" --json` when needed.
  `krill artifacts unshare "<absolute-path>" --json` revokes a file's link;
  `krill artifacts delete "<slug>" --json` deletes a listed artifact.

Honor an explicitly requested format. If the user asks for PDF, Word, slides, or
a spreadsheet, provide that original file and a useful HTML or Markdown artifact
alongside it. Honor requests for local-only or private files without publishing.
Do not publish credentials or private source material as a side effect of an
unrelated task. Editing a repository README or answering a short question does
not by itself call for publishing a document.

Use `krill artifacts --help` for the installed CLI's commands. Artifacts are
listed in Krill's sidebar under Artifacts; no separate browser service or
background file watcher is required.
