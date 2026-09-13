---
name: kokoro-format-and-send
description: Convert rich content into a from-scratch, visual-first lesson with self-checks, complete Markdown, and trackable companion narration, then send it to the local Kokoro Reader when the user explicitly asks to move, send, format, or read something in Kokoro.
---

# Kokoro format and send

Use the bundled `kokoro-reader` MCP server for Kokoro-specific requests such as:

- “send this to Kokoro Reader”
- “move this to Kokoro”
- “format these notes for listening in Kokoro”
- “read this in Kokoro Reader”

Do not activate for ordinary requests to summarize, explain, or read text unless Kokoro Reader is explicitly the destination.

## Choose the delivery lane first

Preserve the complete visual Markdown in every lane. Optimize the companion narration and delivery mechanics, not the user-visible content.

- **Short document:** Use `send_to_reader` when the material has at most eight meaningful reading units and no long code/table-heavy section.
- **Long document:** Use `send_file_to_reader` for multi-section material. Group adjacent short prose under the same real heading into one unit when they teach one idea; keep code, tables, diagrams, equations, and their explanation together. Choose this lane whenever the foundations, examples, or visuals need more than eight units; never compress a lesson merely to stay in the short lane.
- **Teaching narration:** This is the default. Write a manual companion narration for every substantive unit. Sound like a teacher guiding someone who can see the highlighted material: retain each meaningful visual statement in order, paraphrase it naturally in the same tone, then add context, comparison, or connection that makes it easier to understand. Start with plain language and the reason the idea matters before technical detail. The narration is not a mechanical spoken copy, but it should normally equal or exceed the visual material in explanatory content rather than collapse it into a summary.
- **Detailed narration:** Use a longer manual narration only when the user explicitly asks for every detail to be spoken. It may cover all major evidence, but should still explain and group the material instead of reciting it.
- **MCP narration requirement:** Every MCP section must have non-empty custom narration. The Reader rejects missing or blank narration instead of falling back to automatic Markdown-to-speech.

### Beginner-first default

Treat the listener as unfamiliar with the subject unless the user explicitly asks for an advanced treatment. Do not assume they know acronyms, framework names, code concepts, abbreviations, prior context, or why the material matters.

For every substantive document, teach in this order when it applies:

1. Start with what the topic is, the real problem it addresses, and why someone would care.
2. Give one small concrete or everyday example before introducing the mechanism.
3. Introduce each prerequisite in its own reading unit before relying on it later.
4. Define a new term in plain language on first use, explain its role, then introduce its technical name or abbreviation.
5. Build from the simple picture to the detailed process, code, math, comparison, or edge case.

Expand the section count freely when foundations need room. A reader should never need to infer a missing step, silently look up an undefined term, or understand a later diagram before its labels have been introduced. Personalization is optional: use it only when it provides a genuinely helpful example, and never use it to skip the zero-assumption foundation.

### Slow-build teaching rhythm

For a beginner lesson, build every major idea in the same gentle order:

1. State the real-world object or problem in ordinary language and why it matters.
2. Show one tiny concrete example before using the full data, code, equation, or configuration.
3. Name the simple role first, then introduce the technical term in the same unit.
4. Add a small diagram that uses only labels already defined in nearby prose.
5. Explain the mechanism, trade-off, branch, or transformation in more detail.
6. At important transitions, add a short **Pause and predict** self-check whose answer follows from the preceding material; reveal and explain the answer immediately after the question.

Do not jump from a high-level promise directly into identifiers, settings tables, equations, or source code. For example, explain “a point marking an object” before “centroid,” “a line across time” before “edge,” and “a stack of image slices” before a shape such as `(T, Z, Y, X)`. A broad map near the start is useful, but it must be followed by one concept at a time rather than treated as sufficient explanation.

Self-checks create reflection, not a quiz. Use them after a foundational concept, a non-obvious decision, or a branch where a beginner may form a wrong intuition. Ask only a question answerable from material already taught and keep it short. The question is part of the lesson, not decoration: keep it in the visual Markdown **and read the question aloud in the narration**. For every self-check, narration must follow this order: say “Pause and predict,” read the actual question in natural speech, invite a brief thinking pause, then say “The answer is …” and explain both the answer and the misconception it resolves. Never replace the question with a vague phrase such as “pause at the box,” and never reveal the answer before the spoken question. Do not claim the Reader provides clickable or scored interaction.

### Mandatory Codex backlink

Every `send_to_reader` and `send_file_to_reader` call must include `codex_url` explicitly. Build it as `codex://threads/<current-Codex-task-id>` from the current Codex runtime's `CODEX_THREAD_ID` value, then pass that exact URL in the MCP arguments. The MCP server rejects a missing, blank, or malformed backlink; never omit it, invent a thread ID, or rely on the server process environment.

When a shell check is useful, this read-only command prints the value to pass:

```bash
test -n "$CODEX_THREAD_ID" && printf 'codex://threads/%s\n' "$CODEX_THREAD_ID"
```

If the command produces no URL, stop before sending and obtain the current task ID from the Codex runtime. `project_id` remains optional for creating a project and is still the only field used to select an existing project for update.

Manual narration is required whenever the listener needs interpretation rather than literal reading. This includes link-heavy prose, grouped evidence, technical terminology, comparisons, and any section where a teacher would naturally summarize before explaining.

## Prepare the document

1. Use the source material already present in the conversation. Preserve all source details in the visual Markdown. In narration, retain every meaningful statement, important decision, comparison, example, and conclusion in the same order, then add grounded explanation around it. Compress only opaque or repetitive material: commit hashes, run IDs, raw URLs and query strings, repeated source links, repeated boilerplate, and duplicate names with no new meaning. Never omit a value, condition, distinction, or conclusion that changes the reader's understanding.
2. Split it into ordered reading units. Keep a heading with the paragraph it introduces. Group adjacent short prose that shares one decision or concept; keep lists, tables, block quotes, code fences, diagrams, and equations inside the visual unit they explain. Never split inside a fenced code block or table.
3. Put the complete visual representation in each unit's `markdown` field. Preserve headings, lists, tables, links, code, Unicode symbols, and `$...$`/`$$...$$` equations. Use a Unicode box-and-arrow diagram when it materially clarifies relationships that are difficult to follow from the surrounding prose, table, or code alone; the visual-first default below raises that baseline for beginner teaching. Architecture, message flow, sequence, async handoffs, state transitions, retries, fan-out, dependency chains, and branching decisions are good candidates; ordinary prose and already-clear code or tables are not. Keep each diagram beside the text it explains in the same `markdown` field, preserve whitespace and arrows in a fenced `text` block, and do not use HTML or Mermaid.
### Visual-first default

Kokoro deliveries use simple visuals by default. Add at least one fenced `text` Unicode block diagram for every major structure, storage layout, flow, comparison, transformation, or process that benefits from spatial explanation. Do not rely on one generic illustration for the whole document. Keep the prose, tables, code, and source details too: diagrams are additive, never replacements. Use short labels, aligned boxes, and arrows; introduce each label in nearby plain language before relying on it. In narration, walk through each diagram only when it appears, after the prose or code that prepares it, and preserve its meaningful branches and labels. If a concept has no useful relationship to draw, use a small worked example instead of decorative graphics.

Choose the visual shape that matches the idea instead of repeating one arrow diagram:

- Use a small 2-D grid or before/after block map for image pixels, arrays, pooling, masks, and spatial transforms.
- Use a stack, folder tree, or nested boxes for 3-D volumes, file layouts, and ownership.
- Use a timeline for time, frame-by-frame processing, sequences, and sampling windows.
- Use a row-by-column cost table for matching and assignment decisions.
- Use a branch or tree for lineage, fan-out, alternatives, and success/failure paths.
- Use a compact ruler or side-by-side comparison for thresholds, ranges, counts, and trade-offs.

Each visual must answer a specific question the prose makes harder to hold in the head. Prefer several small diagrams beside the relevant steps over one crowded poster at the beginning. Never create decoration without a teaching purpose.

Do not activate this delivery workflow for “summarize this in chat” when Kokoro is not the requested destination.
4. Make narration an anchor-led, track-along teaching guide. **Matching the total section count is necessary but not sufficient.** Before drafting, make a private mirrored outline of every visual unit: prose paragraph, each distinct list item, code block with its explanation, table, diagram, equation, and conclusion. These are ordered **coverage blocks**. Write one narration paragraph for each coverage block, in that same order, separated by a blank line. A heading may share its first content paragraph, and a genuinely repetitive list may share one paragraph, but distinct claims may not be merged. A spoken paragraph must name the block's visible subject or anchor and retain its core claim, condition, value, relationship, or conclusion before adding teaching context. It may then explain why it matters, what it connects to, how it differs from the earlier or later approach, or how it fits the stated platform, baseline, or architecture. Ground added context in the visual source or conversation; label an inference as an inference. Never replace several visual blocks with an overview, preview, or recap.
5. Group only truly repetitive names, examples, or evidence. Omit or compress only opaque metadata: commit hashes, run IDs, raw URLs, repeated source links, duplicated boilerplate, or duplicate names with no new meaning. A list item, paragraph, table row, code branch, equation, or diagram branch with a distinct technical claim must retain its own spoken thought in the same sequence. Do not omit receptive-field math, a stated limitation, a condition, a comparison, a value that determines behavior, or a conclusion. Explain relationships instead of mechanically reading punctuation, but do not skip a meaningful visual step merely because it is easier to summarize. Keep the source's technical vocabulary, confidence, and argument style unless a spoken normalization is needed. Work the heading subject naturally into the opening thought rather than reading the heading as a disconnected title.
6. Keep Markdown literal, but apply **speech normalization** to every manual narration unit so it sounds like an informed person explaining what they have just read. Rewrite visual shorthand into complete spoken phrases according to meaning; do not merely remove punctuation. Never pronounce Markdown fences, backticks, underscores, colons, semicolons, braces, operators, or URL syntax. Translate identifiers into readable words: `cacheKey` becomes “cache key,” `loadCachedAudio` becomes “load cached audio,” `engine_ready` becomes “engine ready,” and `application_eventdelivery_polling_timeout_total` becomes “the Event Delivery polling timeout total metric.” Turn label assignments and separators into relationships: `tenant_id=216185` becomes “the tenant ID is 216185,” and `siteinfo=216185;pm_demo` becomes “the site information pairs 216185 with PM Demo.”
7. For code, teach a trace, not a summary. Begin by naming the file, function, and real source-line range when that information is available in the source. Explain every meaningful line or tightly coupled line group in visible order: inputs, `await` or promise boundaries, guards, branches, calls, state changes, returned values, and error paths. Say what each important expression receives, produces, and hands to the next step. Preserve important indices, values, branches, and output names as anchors. Skip only imports, punctuation, braces, routine conversions, and repeated plumbing; never replace a code block with a vague recap.
8. When a visible call leads to code supplied elsewhere in the source, follow it. First explain the caller through the call site, then transition explicitly: “At line 1031, `loadDocument` is called. Now move to `loadDocument` at lines 663 through 670.” Walk through that callee before returning to the caller's next operation. Keep the caller and callee as adjacent ordered units when they are too dense to explain together. Do not invent a line number: use one only when the source, repository excerpt, or cited file location establishes it; otherwise anchor with the real file and function name.
9. Mention a diagram only when narration reaches that diagram's position in the visual unit. Do not begin with “If you look at this diagram” when prose, code, or a table appears before it. Narrate those earlier elements first, then use one brief transition such as “Now, if you look at the diagram, it shows how the ordered slices move into the model.” Name the diagram type only when useful, use its visible labels as anchors, and follow its main flow or meaningful branches. Do not read drawing glyphs, border characters, or decorative framing. If a unit has no diagram, never mention one.
10. For every visible **Pause and predict** box, narration must read the question itself before the answer. Use a separate spoken paragraph or clearly separated sentences in this order: “Pause and predict,” the full question in plain speech, “take a moment to answer,” then “the answer is …” followed by the explanation. The narration must preserve the question's subject, options, condition, and requested relationship; saying only “pause at the box” does not cover it. If the question appears after a diagram, narrate the diagram first and then read the question.
11. When a visual table, variant list, or diagram presents comparable alternatives, narration must make the useful comparison rather than reciting isolated rows. Name the visual baseline, contrast the meaningful values, and state the implication: larger or smaller weight, wider or narrower coverage, extra augmentation, earlier or later stage, or stronger or weaker contribution. Use exact arithmetic only when it helps and is supported by the visible values. For example, a weight of 0.10 is about one-fifth of 0.55—an absolute difference of 0.45 and roughly 82% lower relative to 0.55—not “a 20% reduction.” Do not invent a causal explanation where the source gives only a difference.
12. Let visual structure, not a shortening target, determine narration length. For substantive material, narration should normally be at least as complete as the visual explanation and often longer because it adds grounded relationships and teaching context. Simple prose can stay short, but do not reduce dense prose, code, tables, diagrams, equations, branches, or self-check questions into a recap. Keep one or two natural sentences for each simple visual paragraph and as many as needed for technical material. Use natural transitions such as “in comparison,” “that means,” and “from there” only where they improve continuity. Prefer complete conversational sentences over row recitation, title repetition, or disconnected fragments. Preserve exact values when they determine behavior. Never invent facts, emotion, or stage directions.

### Speech normalization examples

- **Code identifier rule:** Keep exact identifiers in visual Markdown, but never recite them raw in narration, including line-by-line and caller-to-callee walkthroughs. Split camelCase and PascalCase at word boundaries, expand snake_case, and turn dotted access into a relationship. For example, say “schedule cache preparation” for scheduleCachePreparation, “load document” for loadDocument, “cache key” for cache_key, and “the audio player’s source” for audioPlayer.src. Refer to a raw local path as “the path shown in the visual” unless its components are needed to teach the point.
- **Equation rule:** Keep each equation exact in visual Markdown, but give it a complete plain-language narration rather than reading symbols. First say the question or quantity it represents, then explain its conditioning, left-hand side, and right-hand side or grouped terms in reading order. Expand subscripts, superscripts, time steps, and operators when their meaning is supplied; do not invent meanings that the source does not establish. For example, for p theta of s at time t plus one, o at time t plus one, and r at time t, conditioned on s at time t and a at time t, say: “Given the current state and current action, this asks what probability the model with parameters theta assigns to the next state, the next observation, and the reward at this step.” Then explain why each conditioned and predicted quantity matters before moving on.

- Speak a numeric range with “to” and expand shared shorthand on both sides: `$140K–$200K` becomes “one hundred forty thousand to two hundred thousand dollars,” and `5–10 ms` becomes “five to ten milliseconds.” A leading minus sign still means “negative,” and a subtraction operator still means “minus”; infer the meaning from context.
- Recast an em dash used as punctuation into a natural pause or a connecting phrase rather than saying “dash.”
- Expand units, abbreviations, version labels, and symbols when that improves pronunciation: `v5` becomes “version five,” `20%` becomes “twenty percent,” `0.5×` becomes “zero point five times,” and `A → B` becomes “A leads to B.” Preserve familiar acronyms when their spoken form is clear; otherwise expand them once.
- Keep links complete in visual Markdown. Narration normally omits links entirely and explains the claim they support. If provenance or follow-up matters, mention at most one useful source by its human name, once; later say “the source mentioned earlier” only when a reference is genuinely needed. Never recite a collection of link labels, email subjects, URLs, protocols, query strings, or tracking parameters. For a link-heavy section, summarize the linked material by topic or conclusion rather than naming each link.
- Example: if the visual section lists many Amazon, LinkedIn, Medium, and newsletter links, do not speak those labels. Say something like: “Most of this section is routine promotional and networking mail. The useful distinction is that these are signals to skim, not confirmed job or account updates; keep only messages tied to orders, security, or an action you intend to take.”
- Resolve slashes and compact separators by meaning: `train/test` may become “training and testing,” while `CPU/GPU` may become “CPU or GPU.” Do not apply one mechanical replacement to every slash.

Before using a manual narration unit, read it as standalone plain speech. It should make sense without seeing Markdown punctuation, contain no ambiguous shorthand, and still give enough visible anchors for the listener to track the highlighted section. Check it against the visual paragraph by paragraph: it should teach the same thought at each point, not jump ahead into a broad recap or skip to a later conclusion.

### Coverage-gate example

Treat adjacent but distinct ideas as separate coverage blocks. For example, if the visual first says that VGG stacks three-by-three filters, then gives its receptive-field and parameter comparison, then explains degradation, and finally introduces the ResNet shortcut, narration needs four ordered spoken paragraphs. It must preserve the VGG filter idea, the visible comparison, the degradation limitation, and the shortcut equation or relationship before adding context. Saying only that “later architectures improved VGG” fails the gate: it removes the anchors the reader needs to follow with their eyes.

Likewise, a six-item visual overview needs six ordered spoken thoughts, not one preview sentence that says the topics will be covered later. The listener should be able to read an item, hear that same item named and explained, then move to the next item. A broad summary may introduce or close the unit, but it never substitutes for those matching thoughts.

### Track-along example

Visual Markdown:

```text
MaxSpan v5 has weight 0.55 and uses 64 slices.

Native 384 dense has weight 0.10 and uses the same 64 slices.

MaxSpan v5 reverse has weight 0.15 and adds a horizontal flip.
```

Narration:

“Start with MaxSpan version five. Its weight is 0.55, so this is the main contributor, and it sees 64 slices.

Next, Native 384 dense keeps that same 64-slice coverage but has weight 0.10. Compared with MaxSpan, it is a much smaller complementary vote rather than the primary result.

Finally, MaxSpan version five reverse has weight 0.15. The horizontal flip gives it a different left-right view, which is why it adds diversity instead of simply duplicating the first variant.”

### Expanded teaching example

Visual Markdown:

```text
A small filter is efficient and sees nearby pixels, but it cannot immediately relate distant image regions.

Self-attention compares distant locations, but those global comparisons can be expensive.

Each architecture in the timeline repairs one limitation while introducing the next design problem.
```

Narration:

“A small filter is efficient because it works over nearby pixels and respects local image structure. Its limitation is the same one visible in the sentence: it cannot immediately connect two distant regions, even when they belong to the same action or object.

Self-attention changes that tradeoff. It can compare a location with distant locations and therefore has a global view, but those comparisons become expensive as the image grows. This is the contrast the later vision architectures are trying to manage: local computation is cheap but narrow, while global computation is expressive but costly.

So the timeline is not just a list of model names. Each architecture repairs one side of that tradeoff and exposes the next design problem. CoAtNet belongs at the end because it combines local convolution with global attention in one path, rather than choosing only one of them.”

### Diagram example

For a small architecture explanation whose diagram is the main visual, keep it in the visual Markdown and give a short spoken walkthrough:

```text
╭─────╮     ╭─────────╮     ╭────╮
│ Web ├────▶│ Gateway ├────▶│ DB │
╰─────╯     ╰────┬────╯     ╰────╯
                    │
                    ▼
                ╭────────╮
                │ Orders │
                ╰────────╯
```

Narration: “If you look at this architecture diagram, it shows Web entering through Gateway. Requests go through Gateway, which reads and writes the database. The separate Orders branch handles order work from that same gateway.”

For a processing pipeline, use the same rule:

```text
╭───────╮     ╭────────╮     ╭────────╮
│ Input ├────▶│ Validate├────▶│ Persist│
╰───────╯     ╰───┬────╯     ╰────────╯
                  │ invalid
                  ▼
             ╭────────╮
             │ Reject │
             ╰────────╯
```

Narration: “If you look at this pipeline diagram, it shows validation before persistence. We start with Input and validate it before saving anything. Valid input is persisted; invalid input goes to the Reject path instead.”

More detailed diagrams are welcome when they clarify the source: sequence diagrams, async handoffs, state transitions, retries, fan-out, layered systems, and grouped deployment boundaries all work in a fenced `text` block. For example:

```text
╭────────╮    ╭─────╮    ╭───────╮    ╭────╮
│ Client │───▶│ API │───▶│ Cache │    │ DB │
╰────────╯    ╰──┬──╯    ╰───────╯    ╰────╯
                 │ cache miss             ▲
                 └────────────────────────┘
```

Narration: “If you look at this sequence diagram, it shows the API checking the cache before going to the database. The client calls the API first. On a cache miss, the API fetches the data from the database instead.”

For a larger diagram, keep all meaningful boxes and arrows visible, but narrate only the main flow and branches needed to follow it. A grouped boundary such as `core` is worth naming only when it explains ownership or a deployment boundary.

When prose or code comes before a diagram, preserve that order in narration. Explain the prose or code first; only then say, for example: “Now, if you look at the flow diagram, it summarizes that calculation as orientation and physical position producing an ordered MRI stack.” Do not pull the diagram explanation to the beginning of the unit.

### Code walkthrough example

Keep the complete snippet visible. The narration should teach the reader how its named values relate, instead of paraphrasing the block as one vague step:

```python
row_direction = np.asarray(image_orientation[:3])
column_direction = np.asarray(image_orientation[3:])
slice_normal = np.cross(row_direction, column_direction)
sort_key = np.dot(physical_position, slice_normal)
```

Narration: “If you look at this pseudocode, it calculates where each MRI slice sits along its stack. First, NumPy converts the first three image-orientation values into the row-direction array, and stores the remaining three values in the column-direction array. The NumPy cross function takes those two directions and produces their perpendicular direction, stored in slice normal. Finally, the dot product combines the slice’s physical position with that normal direction to create sort key: one number we can use to order the slices anatomically.”

### Repository code drill-down

For real repository code, make file and line anchors visible whenever they are known. A code section may begin with `src/main.ts, lines 663–670 — loadDocument`, followed by the exact snippet. The narration must use those same anchors and follow control flow instead of summarizing: “At line 663, `loadDocument` begins. Line 664 awaits the backend `get_document` command, so the function pauses until the Reader returns either a project or no project. Lines 665 through 668 store that result in the current UI state; the null fallback deliberately clears the project ID and document. At line 669, `renderDocument` redraws the selector and reading view using that new state.” If the next visible call is `renderDocument`, continue with its actual file and line range in the next unit and explain it before moving on. Never fabricate file names or line numbers; when they are unavailable, name the function and the exact visible expression instead.

### Comparison example

When a visible table compares model variants, use the values to explain their relative role:

| Variant | Weight | Augmentation |
| --- | ---: | --- |
| MaxSpan v5 | 0.55 | none |
| Native 384 dense | 0.10 | none |
| MaxSpan v5 reverse | 0.15 | horizontal flip |

Narration: “Look at the variant table. MaxSpan version five is the dominant contributor at weight 0.55. Native 384 dense is only 0.10, about one-fifth as much, so it acts as a smaller complementary vote rather than the main result. MaxSpan version five reverse sits between them at 0.15 and adds a horizontal flip, giving the ensemble a different left-right view. The table shows a deliberate mix: one primary variant plus smaller alternatives that add diversity.”

## Build once, validate once, then send

Each send creates a new isolated Reader project unless its optional `project_id` is supplied. Keep the returned `project_id` when the user asks to revise the same document. Both transfer tools require the explicit `codex_url` backlink described above. Use `list_reader_projects` to discover existing projects, and `get_reader_project_location` to find a project's editable `source.md` and `narration.txt`. Use `delete_reader_project` only after the user explicitly approves permanently deleting that exact project.

For short content, call `send_to_reader` with one visual unit per `sections` entry, non-empty manual teaching narration for every section, and the required `codex_url`. Omit `project_id` for a new document; supply it only to update the chosen existing project.

For longer or multi-section content, create one uniquely named source file and one matching narration file under `~/.kokoro_reader/inbox/`. Build the complete packet before the first write; do not write an initial document and then reread or rewrite it merely to add headings, separators, or narration. Call `send_file_to_reader` with the required `codex_url`, without `project_id` for a new project, or with a selected existing `project_id` to replace that project's document.

- `<slug>-source.md`: ordered paragraph-level visual Markdown units. Put `<!-- kokoro-reader-section -->` on its own line before every unit, including units under the same heading.
- `<slug>-narration.txt`: required matching narration units, separated by `---` on its own line. Write non-empty teacher-style narration for every section; the MCP server rejects an omitted file, a mismatched count, or an empty section.

When material arrives across multiple sessions, use `append_to_reader_files` to add one complete
section or a grouped chunk to the matching staging pair. Pass the returned section count as
`expected_section_count` on the next call when available. The tool owns line-ending normalization,
outer-whitespace trimming, canonical separators, and pair synchronization; it does not invent
missing narration or repair semantic content and URLs. Do not call it once per paragraph or email.
After the full packet is ready, call `precheck_reader_files` once and then call
`send_file_to_reader` once. Staging calls do not create, focus, or refresh a Reader project.

Before writing, count the planned visual and narration units. After the one write, run one marker-count check: the number of source markers must equal the same number of non-empty narration sections. Then apply the **coverage gate** to every manual unit: count its meaningful coverage blocks and its narration paragraphs. The counts must match unless a heading shares its first paragraph or the visual material is truly repetitive metadata. For each block, confirm its narration paragraph contains the same visible subject and core claim; restore any missing block before sending. A later summary, preview, or broad conclusion never counts as a counterpart. Compress only opaque identifiers and true repetition. For a code unit, confirm that narration covers every meaningful visible line or line group in order, uses real file/line anchors where supplied, follows each supplied callee after its call site, and explains the purpose, inputs, operations, state changes, output, and error path. For a table or variant comparison, check that manual narration states at least one useful relative relationship and its supported implication. For a unit containing a diagram, verify that the fenced `text` diagram stays in that visual unit and that manual narration mentions it only where it appears. Call the Reader once, then verify that `accepted_sections` equals the planned count. If it does not, fix the separators and retry once. An unmarked source is accepted as a paragraph-split fallback, but explicit markers are required when exact alignment with a narration file matters.

Never edit a project's `document.json` directly. After `get_reader_project_location`, a user can edit that project's `source.md` and `narration.txt`, select the same project in the app, and choose **Reload files**.

Both tools activate the selected project, retain one recovery copy within that project, focus the app, switch to Listen mode, highlight the first section, and do not autoplay. Tell the user only that it was sent after the tool succeeds; do not claim audio was played automatically.
