# Personal context for finding things

Status: proposed 2026-10-24. M2, gated on the M1 trial verdict (T08), like the other M2 work. Not on the M1 task graph.

All names below are synthetic. Do not add a real person's name, a real project or employer name, or a real pet's name to this document or its fixtures.

## Problem

Notes are found by their own words, but people search with words the note never used:

| Capture | What the person later searches | Why the plain words miss it |
| --- | --- | --- |
| "Tell Dana about Biscuit" | "therapy" | Dana is the person's therapist, and the note never says "therapy". |
| "I need to deploy ORCA tomorrow" | "ORCA", or "orca stuff" | Voice transcribes it as "Orca", "or ca" or "O R C A". |
| "Biscuit threw up again" | "the dog" | Biscuit is the dog. |

The person knows these facts and the app does not. This feature lets the person record a few of them and uses them **only when searching**.

## Decision: expand searches on the device; don't tag at capture

When a search matches a context entry's name, alias, relationship or linked session, the search is expanded locally to that entry's name and aliases. This uses the existing on-device index and the existing search-phrase rules (`core/src/retrieval/index/`, `core/src/retrieval/phrases/`).

Why this and not tagging each capture:
- **Retroactive by construction.** It works on every existing note from the moment an entry is added, with no reprocessing and no provider calls. Editing or deleting an entry changes future searches immediately and leaves no derived residue.
- **No disclosure.** Context entries are never rendered into an interpretation request, so no provider sees "Dana is my therapist". The work/personal provider setup and credential availability do not matter here.
- **No new model authority.** The model is not in the loop, so it cannot invent a project, attach a note to the wrong person or change scope. Nothing about the M1 interpretation contract, its instructions or its guards changes.
- **Explainable.** Every expanded result can say why it matched.

It does not set work or personal scope. Scope stays a privacy boundary that is chosen when the note is captured and changed only by an explicit correction (DESIGN.md, "Privacy, ownership, and lifecycle"). A project marked as work does not move a personal note to work.

## Context entries

A short, typed list that the person maintains. There is no free-text "about me" field.

| Kind | Fields | Example |
| --- | --- | --- |
| Person | name, aliases, relationship, optional linked session | Dana; aliases "Dayna", "Donna"; therapist; linked session "therapy" |
| Pet | name, aliases, what it is | Biscuit; aliases "Bisquit"; dog |
| Project | name, aliases | ORCA; aliases "Orca", "or ca", "O R C A" |

Every entry has a **side**, personal or work, which is set when the entry is created.

Aliases matter more than they look. Voice transcription mangles proper nouns, and without aliases most voice captures would miss. An alias may contain several words, and it is matched as a phrase.

## Search behavior

1. **What expands.** A search term expands when it equals, ignoring case and extra whitespace:
   - an entry's name or one of its aliases;
   - an entry's relationship or kind ("therapist", "dog");
   - a session linked from an entry ("therapy").
2. **What it expands to.** The search becomes the original words OR the entry's name OR any of its aliases.
   - A linked session also includes items whose session topic is that session, through the existing session-topic filter.
   - All other filters (type, date, lifecycle, scope) still apply unchanged.
3. **Results.** Each result that matched only through an expansion carries a "matched via" reason, for example "Dana (therapist)". The person can turn expansion off for one search.
4. **Scope.** An entry expands only searches whose read scope includes its side. Work entries never expand a personal search, and personal entries never expand a work search. Retrieval is personal-only today (`core/src/retrieval/query/mod.rs`), so in practice personal entries apply.
5. **Bounds.**
   - Expansion is one level deep: an alias does not expand into other entries' aliases.
   - Each search is capped at a fixed number of added terms.
   - A search that matches no entry runs exactly as today.

## Lifecycle and privacy

- Entries live in the core's protected store, alongside the person's notes.
- They are covered by export, deletion and reset, like any other record.
- They never appear in a notification payload, a log, a metric or an interpretation request.
- Each entry has a revision, and edits are ordinary corrections.
- Deleting an entry removes it. Because nothing was derived from it, nothing else needs cleaning up.

## Not in scope

- Tagging or classifying a note when it is captured.
- Changing scope.
- Resolving pronouns: "tell her about Biscuit" is not found under "therapy".
- Disambiguating two people with the same name. The "matched via" reason makes the overlap visible.
- Suggesting aliases automatically.
- Any model involvement.

## Possible later extension: tagging at capture

Build this only if the trial and M2 show that searching is not enough, for example for grouping views that need stored tags. Constraints recorded now, so a later design starts from them:
- The model may only reference existing entry ids, each with a quoted source span. Core accepts a reference only when the entry exists, its side is permitted for the request's destination, and the span contains the entry's name or an alias. This is the same pattern as the reminder guard.
- An entry is rendered into a request only when the capture text matches its name or an alias on the device, and only when the destination is permitted for the entry's side. That device-side match only reduces what is sent; it never decides scope.
- Tags are derived and replaceable. A person's removal of a tag survives reprocessing.
- Reprocessing past notes is optional and limited by the person. One option is to reprocess only notes whose text matches the new entry's names, within a chosen date range, with the count and cost shown before it runs.

## Acceptance sketch for the eventual tasks

1. **Core: entries and their lifecycle.**
   - Create, edit and delete with revisions.
   - Covered by export, deletion and reset.
   - Tests prove that no entry text reaches an interpretation request, a notification payload or a metric.
2. **Core: search expansion.**
   - Tests for each fixture in the problem table, using synthetic notes:
     - a name match, an alias match, a relationship match and a linked-session match;
     - an entry on the other side that does not expand;
     - a search that matches no entry and runs unchanged;
     - an edit or deletion that takes effect on the next search;
     - the term cap.
   - Expanded results carry their reason.
3. **App: managing entries.** A screen in Settings to add, edit and delete entries. Following `docs/features/m1-ui-design.md`, it is never a required setup step.
4. **App: search results.** Results show the "matched via" reason, and expansion can be turned off for one search.
