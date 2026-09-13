import { describe, expect, test } from "bun:test";
import { deriveSpeechText, groupSpeechChunks, reconcileSections, splitMarkdownSections, splitSentences } from "./text";

describe("text preparation", () => {
  test("preserves fenced code as one Markdown section", () => {
    expect(splitMarkdownSections("# Heading\n\n```ts\nconst x = 1;\n\nconst y = 2;\n```\n\nDone.")).toEqual([
      "# Heading",
      "```ts\nconst x = 1;\n\nconst y = 2;\n```",
      "Done.",
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

  test("keeps custom narration when an unchanged section moves", () => {
    const oldSections = [
      { markdown: "# Intro", speech_text: "Intro", speech_mode: "automatic" as const },
      { markdown: "Details", speech_text: "Custom details", speech_mode: "custom" as const },
    ];
    const sections = reconcileSections(["Details", "# Intro"], oldSections);
    expect(sections[0]).toEqual(oldSections[1]);
    expect(sections[1]).toEqual(oldSections[0]);
  });
});
