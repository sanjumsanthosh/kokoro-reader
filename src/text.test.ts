import { describe, expect, test } from "bun:test";
import { bestMarkdownPassage, deriveSpeechText, groupSpeechChunks, literalTextMatches, narrationHighlights, reconcileSections, speechChunkRanges, splitMarkdownSectionRanges, splitMarkdownSections, splitSentences } from "./text";

describe("text preparation", () => {
  test("preserves fenced code as one Markdown section", () => {
    expect(splitMarkdownSections("# Heading\n\n```ts\nconst x = 1;\n\nconst y = 2;\n```\n\nDone.")).toEqual([
      "# Heading",
      "```ts\nconst x = 1;\n\nconst y = 2;\n```",
      "Done.",
    ]);
  });

  test("preserves explicit Reader card markers instead of splitting paragraphs", () => {
    const source = "# First\n\nOne paragraph.\n\n<!-- kokoro-reader-section -->\n\n# Second\n\nAnother paragraph.";
    expect(splitMarkdownSectionRanges(source)).toEqual([
      { text: "# First\n\nOne paragraph.", start: 0, end: 23 },
      { text: "# Second\n\nAnother paragraph.", start: 57, end: 85 },
    ]);
  });

  test("converts visual Markdown symbols to spoken words", () => {
    expect(deriveSpeechText("**x** → y, where $x \\leq y$.")).toContain("leads to");
    expect(deriveSpeechText("**x** → y, where $x \\leq y$.")).toContain("less than or equal to");
  });

  test("segments ordinary sentences", () => {
    expect(splitSentences("First sentence. Second sentence!")).toEqual(["First sentence.", "Second sentence!"]);
  });

  test("groups teaching sentences without crossing the size limit", () => {
    const chunks = groupSpeechChunks(
      "We load the cache first. The cached value is stored in cached. If it exists, we return it immediately. Otherwise, speech generation continues. The next step prepares the audio player.",
      20,
    );
    expect(chunks).toEqual([
      "We load the cache first. The cached value is stored in cached. If it exists, we return it immediately.",
      "Otherwise, speech generation continues. The next step prepares the audio player.",
    ]);
  });

  test("merges a short trailing sentence into the previous chunk", () => {
    expect(groupSpeechChunks("This is a sufficiently long first sentence for context. End.", 12, 10)).toEqual([
      "This is a sufficiently long first sentence for context. End.",
    ]);
  });

  test("keeps narration paragraphs in separate chunks", () => {
    expect(groupSpeechChunks("First paragraph stays together.\n\nSecond paragraph starts a new clip.")).toEqual([
      "First paragraph stays together.",
      "Second paragraph starts a new clip.",
    ]);
  });

  test("keeps custom narration when an unchanged section moves", () => {
    const oldSections = [
      { markdown: "# Intro", speech_text: "Intro", speech_mode: "automatic" as const },
      { markdown: "Details", speech_text: "Custom details", speech_mode: "custom" as const },
    ];
    const sections = reconcileSections(["Details", "# Intro"], oldSections);
    expect(sections[0]).toEqual(oldSections[1]);
    expect(sections[1]).toEqual(oldSections[0]);
  });

  test("finds case-insensitive literal matches", () => {
    expect(literalTextMatches("Regex? REGEX?", "regex?")).toEqual([
      { start: 0, end: 6 },
      { start: 7, end: 13 },
    ]);
    expect(literalTextMatches("Nothing", "")).toEqual([]);
  });

  test("finds exact phrase ranges including connecting words", () => {
    const block = "The One-Time code is a temporary login secret, not a receipt.";
    const [match] = narrationHighlights([block], "A one-time code is a temporary login secret.");
    expect(match).toMatchObject({ blockIndex: 0, mode: "exact" });
    expect(match.ranges.map((range) => block.slice(range.start, range.end))).toEqual(["One-Time code is a temporary login secret"]);
  });

  test("finds exact ranges in each relevant visible block", () => {
    expect(narrationHighlights([
      "HostingRaja login codes need review.",
      "Cloudflare crawler settings need review.",
      "The system is ready.",
    ], "HostingRaja login codes need review. Cloudflare crawler settings need review. The model is ready.")).toMatchObject([
      { blockIndex: 0, mode: "exact" },
      { blockIndex: 1, mode: "exact" },
    ]);
  });

  test("falls back to meaningful overlap in the best visible block", () => {
    expect(narrationHighlights([
      "A personal agent reads email and can make purchases.",
      "Sentinel checks permissions before an action leaves the device.",
      "Unrelated historical background appears here.",
    ], "Sentinel validates permissions for a device action.")).toEqual([
      { blockIndex: 1, ranges: [], terms: ["sentinel", "permissions", "action", "device"], mode: "fallback" },
    ]);
  });

  test("does not highlight stop words or one-word coincidence", () => {
    expect(narrationHighlights(["The agent is ready."], "The model is ready.")).toEqual([]);
  });

  test("uses exact ranges before common-word fallback", () => {
    expect(narrationHighlights([
      "Sentinel checks permissions before an action leaves the device.",
      "The user approves actions through a system dialog.",
    ], "Sentinel checks permissions before an action leaves the device. The user approves actions.")).toEqual([
      { blockIndex: 0, ranges: [{ start: 0, end: 62 }], terms: ["sentinel", "checks", "permissions", "before", "action", "leaves", "device"], mode: "exact" },
      { blockIndex: 1, ranges: [{ start: 0, end: 25 }], terms: ["user", "approves", "actions"], mode: "exact" },
    ]);
    expect(narrationHighlights(["It does not do this and can talk about it."], "The model does not do it and can talk about it.")).toEqual([]);
  });

  test("returns narration chunk offsets and the closest Markdown paragraph", () => {
    const narration = "First passage.\n\nSecond passage explains storage.";
    expect(speechChunkRanges(narration)).toEqual([
      { text: "First passage.", start: 0, end: 14 },
      { text: "Second passage explains storage.", start: 16, end: 48 },
    ]);
    const source = "# First\n\nSecond passage explains **storage** clearly.";
    expect(bestMarkdownPassage(source, "Second passage explains storage.")).toEqual({ start: 9, end: source.length });
  });
});
