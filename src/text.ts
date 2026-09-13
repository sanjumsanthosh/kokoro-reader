interface SentenceSegment {
  segment: string;
}

interface SentenceSegmenter {
  segment(input: string): Iterable<SentenceSegment>;
}

interface SentenceSegmenterConstructor {
  new (locale: string, options: { granularity: "sentence" }): SentenceSegmenter;
}

export type SpeechMode = "automatic" | "custom";

export interface NarrationSection {
  markdown: string;
  speech_text: string;
  speech_mode: SpeechMode;
}

export function splitMarkdownSections(source: string): string[] {
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  const sections: string[] = [];
  let current: string[] = [];
  let fenced = false;
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed.startsWith("```") || trimmed.startsWith("~~~")) fenced = !fenced;
    if (!fenced && trimmed === "" && current.length > 0) {
      sections.push(current.join("\n").trim());
      current = [];
    } else {
      current.push(line);
    }
  }
  if (current.join("\n").trim()) sections.push(current.join("\n").trim());
  return sections;
}

export function deriveSpeechText(source: string): string {
  let text = source
    .replace(/<[^>]*>/g, " ")
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s*#{1,6}\s*/gm, "")
    .replace(/^\s*[-*+]\s+/gm, "")
    .replace(/^\s*>\s?/gm, "")
    .replace(/```[^\n]*|```/g, "")
    .replace(/[`*_~]/g, "");
  const replacements: Array<[string, string]> = [
    ["\\rightarrow", " leads to "],
    ["\\Rightarrow", " implies "],
    ["\\leftrightarrow", " corresponds to "],
    ["\\leq", " less than or equal to "],
    ["\\geq", " greater than or equal to "],
    ["\\times", " times "],
    ["\\cdot", " times "],
    ["\\pm", " plus or minus "],
    ["\\approx", " approximately "],
    ["→", " leads to "],
    ["⇒", " implies "],
    ["↔", " corresponds to "],
    ["≤", " less than or equal to "],
    ["≥", " greater than or equal to "],
    ["×", " times "],
    ["±", " plus or minus "],
    ["≈", " approximately "],
  ];
  for (const [from, to] of replacements) text = text.replaceAll(from, to);
  return text.replace(/[${}\\]/g, " ").split(/\s+/).filter(Boolean).join(" ");
}

export function reconcileSections(
  blocks: string[],
  oldSections: ReadonlyArray<NarrationSection>,
): NarrationSection[] {
  const used = new Set<number>();
  return blocks.map((block, index) => {
    const samePosition = oldSections[index];
    const samePositionIndex = samePosition?.markdown === block ? index : -1;
    const matchedIndex = samePositionIndex >= 0
      ? samePositionIndex
      : oldSections.findIndex((section, oldIndex) => !used.has(oldIndex) && section.markdown === block);
    if (matchedIndex >= 0) {
      used.add(matchedIndex);
      return oldSections[matchedIndex];
    }
    return {
      markdown: block,
      speech_text: deriveSpeechText(block),
      speech_mode: "automatic",
    };
  });
}

export function splitSentences(source: string): string[] {
  const segmenterConstructor = (Intl as unknown as { Segmenter?: SentenceSegmenterConstructor }).Segmenter;
  if (segmenterConstructor) {
    return Array.from(new segmenterConstructor("en", { granularity: "sentence" }).segment(source))
      .map(({ segment }) => segment.trim())
      .filter(Boolean);
  }
  return (source.match(/[^.!?]+[.!?]+|[^.!?]+$/g) ?? []).map((sentence) => sentence.trim()).filter(Boolean);
}

function wordCount(source: string): number {
  return source.split(/\s+/).filter(Boolean).length;
}

export function groupSpeechChunks(source: string, maxWords = 80, trailingMergeWords = 10): string[] {
  const sentences = splitSentences(source);
  const chunks: string[] = [];
  let current = "";
  for (const sentence of sentences) {
    const candidate = current ? `${current} ${sentence}` : sentence;
    if (current && wordCount(candidate) > maxWords) {
      chunks.push(current);
      current = sentence;
    } else {
      current = candidate;
    }
  }
  if (current) chunks.push(current);
  if (chunks.length > 1) {
    const last = chunks[chunks.length - 1];
    const previous = chunks[chunks.length - 2];
    if (wordCount(last) < trailingMergeWords && wordCount(`${previous} ${last}`) <= maxWords) {
      chunks.splice(chunks.length - 2, 2, `${previous} ${last}`);
    }
  }
  return chunks;
}
