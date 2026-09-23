---
name: kokoro-format-and-send
description: Create a beginner-friendly, visual-first Kokoro Reader lesson with eye-trackable custom narration, then send or append it when the user explicitly names Kokoro Reader.
---

# Kokoro format and send

Use this skill only when Kokoro Reader is the explicit destination. Do not use it for an ordinary chat summary.

## Contract

Trigger for requests such as “send this article to Kokoro,” “make this readable in Kokoro,” or “append this to my Kokoro lesson.” Do not trigger for “summarize this in chat.”

Create a visual lesson and matching custom narration. The Reader project is the only mutation; never delete a project without the user’s explicit approval. If the source, current task backlink, or required narration is unavailable, stop before sending and explain the blocker.

## Preserve requested source formats

When the user explicitly names another skill, document format, or artifact contract, keep that source format visible in the Reader. Kokoro formatting may add matching narration and split the material into aligned cards, but it must not replace required source structure with a summary.

When transferring an explanation produced in the current task, inventory its headings, command blocks, code blocks, diagrams, checkpoints, and conclusions before drafting Reader cards. Every material item must appear in the Reader source in the same order. Do not shorten, omit, or replace any item with a recap unless the user explicitly asks for a summary. If the inventory needs more than eight cards, use staged files; the card limit is never a reason to compress the lesson.

For `repository-aware-diff-explainer`, the visible Reader lesson must retain its required Phase 0 through Phase 3 sections: repository file map, architectural observation, change story, reader orientation when applicable, every meaningful behavior-changing hunk, source-language code fences with real line numbers, `-` and `+` line markers, inline before/after annotations, an Example Checkpoint after each hunk, cross-file behavior flow, before-versus-after summary, risks or verification, and skipped-but-worth-noticing notes. Do not substitute pseudo-code, omit removed lines, collapse line annotations into prose, or send a high-level feature overview in place of that artifact.

Before transfer, run a visible-contract check: each meaningful hunk has both the required old and new lines where the diff contains both, each changed line has its inline rationale, every hunk has its checkpoint, all required ending sections are present, and the source-to-Reader inventory has no material omissions. If the artifact exceeds eight cards, use staged source and narration files with precheck rather than dropping content.

## Alignment comes first

One Reader section is one **teaching card**: one idea, its optional diagram, and one matching narration unit. Do not group unrelated coverage blocks merely to reduce section count.

For every card, use this visible order when applicable:

1. A short heading that names the idea.
2. The explanation and any important values or conditions.
3. One optional fenced `text` diagram with a visible caption that explains every meaningful path or label.
4. `**Connection to what you already know:**` for a grounded everyday or current-practice comparison.
5. `**Pause and predict:**` followed immediately by a visible `**Answer:**`.

Narration must follow the same visible order. It may explain in natural speech, but must retain the same named subjects, numbers, conditions, and conclusion. Every spoken explanation, connection, and self-check answer must be visible in the card. Do not add narration-only claims. Do not leave a visible claim, diagram branch, list item, or answer unexplained.

Before transfer, compare each card against its narration paragraph by paragraph. A card is invalid when it has fewer than two meaningful shared terms, when its narration introduces a new claim, or when an important visual block lacks a matching spoken thought. Run the Identifier Read-Aloud Gate in [speech.md](references/speech.md) before sending; narration containing raw identifier syntax is invalid.

## Natural narration is a hard gate

The precheck ready flag is necessary but not sufficient. Never copy, transform, or mechanically derive narration from visual Markdown to improve alignment. Narration must sound like a person explaining the card without its formatting: no headings, tables, fences, diff markers, raw paths, raw identifiers, or source-code syntax.

Before transfer, read each narration paragraph by itself. It must be conversational, explain rather than recite, preserve the visible example and conclusion, and say identifiers as ordinary words. If this review fails, do not send.

If natural narration fails alignment, preserve the same visual/narration packet and repair only the affected card. Make up to three targeted rewrite-and-precheck passes while the diagnostics improve. Do not add copied text or raw code syntax to narration. If the remaining findings are verified false positives after those repairs, use the existing acknowledgement flow with a concrete reason and semantic-coverage checks; otherwise report the unresolved genuine omission.

## Teach from zero

Start with the real-world problem, then give a small example before technical terms. Define a term on first use. Use a diagram only when it makes a relationship easier to hold in mind; diagrams are additive, not decoration. Keep a heading with the text it introduces.

Use manual narration for every section. Call `get_reader_pronunciations` once before drafting and apply its entries only in narration. Normalize Markdown and identifiers into natural speech, but retain visible names and values closely enough for eye tracking.

For code-focused cards that explain transformations, parsing, conditions, data flow, or calls, use one continuing worked example. Put the visible **Example input** before exact code and real line numbers; show each meaningful line or coupled group with the example's concrete state; add a compact input → intermediate state → output diagram or table; then name the **Example output** and next consumer. Carry the same example into an adjacent card when the execution path continues. Do not invent an example for imports, declarations, schemas, or other non-transforming lines. Narration must cover the input, each state change, output, and next consumer in that same visible order.

Read every self-check in this order: “Pause and predict,” the full question, a brief thinking invitation, then the visible answer and why it is correct.

For diagrams, code, tables, equations, or high-volume lists, read the relevant reference before drafting:

- [visuals.md](references/visuals.md)
- [code-and-lists.md](references/code-and-lists.md)
- [speech.md](references/speech.md)

## Send safely

Build `codex_url` as `codex://threads/<CODEX_THREAD_ID>` and pass it explicitly to every send.

Treat the first accepted project ID, revision, source path, and narration path as the draft identity. On a correction, keep that project: fetch its current location and revision, update only the affected complete cards with `update_reader_project_sections`, and reread before retrying a revision conflict. Never delete or recreate a project merely to revise its narration. Deletion requires the user's explicit approval.

The project-local `source.md` and `narration.txt` are the canonical editable pair after transfer. Do not leave competing revised copies in the inbox or repository. For an external editor, save both files, let the Reader reload them, and resolve any shown conflict before sending another update.

Use `send_to_reader` for eight or fewer teaching cards. For longer lessons, create matching `<slug>-source.md` and `<slug>-narration.txt` staging files under `~/.kokoro_reader/inbox/`, with `<!-- kokoro-reader-section -->` before every visual card and `---` between narration cards. For an append, copy or rebuild the complete existing packet into staging, append complete cards, precheck once, then update the selected project.

For compact, uniform structured cards, prefer `send_toon_packet`: provide a strict TOON packet with a title and cards containing `id`, `heading`, `claim`, `narration`, plus optional connection, diagram, and pause/answer fields. The MCP renders normal Markdown and custom narration, then rejects weak alignment before transfer. Keep `send_to_reader` and file transfer for rich Markdown, code, equations, or bespoke visual layouts where TOON does not reduce repeated structure.

For a persistent Gmail triage batch, prefer `send_gmail_digest_toon`. Supply the saved batch ID and strict TOON clusters with their ordered message IDs, verdict, concise narrative, narration, and optional primary source. The Reader resolves Gmail permalinks from the manifest, requires every message exactly once, and renders routine rows without exposing message bodies. Do not use this tool for arbitrary Gmail paths or individual mail bodies.

Run `precheck_sections` for direct packets and `precheck_reader_files` for staged packets once the packet is complete. If findings remain, first fix genuine omissions or unreadable narration through the targeted recovery loop. An agent may override only false-positive or non-material advisory findings by sending the current `override_token` with a concrete reason and `verified_checks`; it must verify visible coverage, each required semantic code group, natural expansion of glued identifiers such as `MoneyControl` into “Money Control,” narration-only readability, and each remaining diagnostic. Never override malformed files, blank narration, section-count mismatch, size, path, backlink, project, or revision failures. State in the final response that the lesson was sent with acknowledged precheck findings, list the findings and checks, and never call that a clean pass. Then verify `accepted_sections` equals the planned card count.

The Reader switches to Listen mode but does not autoplay. Say it was sent only after the transfer succeeds.
