interface SentenceSegment {
  segment: string;
}

interface SentenceSegmenter {
  segment(input: string): Iterable<SentenceSegment>;
}

interface SentenceSegmenterConstructor {
  new (locale: string, options: { granularity: "sentence" }): SentenceSegmenter;
}

interface WordSegment {
  segment: string;
  isWordLike: boolean;
  index: number;
}

interface WordSegmenter {
  segment(input: string): Iterable<WordSegment>;
}

interface WordSegmenterConstructor {
  new (locale: string, options: { granularity: "word" }): WordSegmenter;
}

export type SpeechMode = "automatic" | "custom";

export interface NarrationSection {
  markdown: string;
  speech_text: string;
  speech_mode: SpeechMode;
}

export interface TextMatch {
  start: number;
  end: number;
}

export interface MarkdownSectionRange extends TextMatch { text: string; }

export interface NarrationHighlight {
  blockIndex: number;
  ranges: TextMatch[];
  terms: string[];
  mode: "exact" | "fallback";
}

export interface SpeechChunkRange extends TextMatch {
  text: string;
}

interface WordToken extends TextMatch {
  normalized: string;
  meaningful: boolean;
}

const STOP_WORDS = new Set([
  "a", "about", "an", "and", "are", "as", "at", "be", "by", "can", "did", "do", "does", "for", "from", "has", "have", "in", "is", "it", "not", "of", "on", "or", "that", "the", "their", "this", "to", "was", "with", "you", "your",
]);

function normalizeWord(word: string): string {
  return word.normalize("NFKC").toLocaleLowerCase("en");
}

function isMeaningfulWord(word: string): boolean {
  return !STOP_WORDS.has(word) && (word.length >= 3 || /^\d+$/u.test(word));
}

function wordTokens(source: string): WordToken[] {
  const segmenterConstructor = (Intl as unknown as { Segmenter?: WordSegmenterConstructor }).Segmenter;
  const rawWords = segmenterConstructor
    ? Array.from(new segmenterConstructor("en", { granularity: "word" }).segment(source))
      .filter(({ isWordLike }) => isWordLike)
      .map(({ segment, index }) => ({ segment, start: index, end: index + segment.length }))
    : Array.from(source.matchAll(/[\p{L}\p{N}]+/gu)).map((match) => ({
      segment: match[0],
      start: match.index ?? 0,
      end: (match.index ?? 0) + match[0].length,
    }));
  return rawWords.map(({ segment, start, end }) => {
    const normalized = normalizeWord(segment);
    return { start, end, normalized, meaningful: isMeaningfulWord(normalized) };
  });
}

function meaningfulWords(source: string): string[] {
  return wordTokens(source).filter(({ meaningful }) => meaningful).map(({ normalized }) => normalized);
}

function exactRanges(visible: string, narration: string): TextMatch[] {
  const visibleTokens = wordTokens(visible);
  const narrationTokens = wordTokens(narration);
  const candidates: TextMatch[] = [];
  for (let visibleIndex = 0; visibleIndex < visibleTokens.length; visibleIndex += 1) {
    for (let narrationIndex = 0; narrationIndex < narrationTokens.length; narrationIndex += 1) {
      if (visibleTokens[visibleIndex].normalized !== narrationTokens[narrationIndex].normalized) continue;
      if (visibleIndex > 0 && narrationIndex > 0 && visibleTokens[visibleIndex - 1].normalized === narrationTokens[narrationIndex - 1].normalized) continue;
      let length = 0;
      while (
        visibleIndex + length < visibleTokens.length
        && narrationIndex + length < narrationTokens.length
        && visibleTokens[visibleIndex + length].normalized === narrationTokens[narrationIndex + length].normalized
      ) length += 1;
      const run = visibleTokens.slice(visibleIndex, visibleIndex + length);
      if (length >= 2 && run.some(({ meaningful }) => meaningful)) {
        candidates.push({ start: run[0].start, end: run[run.length - 1].end });
      }
    }
  }
  const ranges: TextMatch[] = [];
  for (const candidate of candidates.sort((left, right) => left.start - right.start || right.end - left.end)) {
    if (!ranges.some((range) => candidate.start < range.end && candidate.end > range.start)) ranges.push(candidate);
  }
  return ranges;
}

export function narrationHighlights(visibleBlocks: string[], narration: string): NarrationHighlight[] {
  const narrationTerms = new Set(meaningfulWords(narration));
  const candidates = visibleBlocks.map((block, blockIndex) => {
    const words = meaningfulWords(block);
    const terms = [...new Set(words.filter((word) => narrationTerms.has(word)))];
    let adjacentPairs = 0;
    for (let index = 1; index < words.length; index += 1) {
      if (narrationTerms.has(words[index - 1]) && narrationTerms.has(words[index])) adjacentPairs += 1;
    }
    return { blockIndex, terms, score: terms.length + adjacentPairs * 2 };
  }).filter(({ terms }) => terms.length >= 2);
  const exact = candidates.flatMap(({ blockIndex, terms }) => {
    const ranges = exactRanges(visibleBlocks[blockIndex], narration);
    return ranges.length ? [{ blockIndex, ranges, terms, mode: "exact" as const }] : [];
  });
  if (exact.length) return exact;
  return candidates
    .sort((left, right) => right.score - left.score || left.blockIndex - right.blockIndex)
    .slice(0, 1)
    .map(({ blockIndex, terms }) => ({ blockIndex, ranges: [], terms, mode: "fallback" as const }));
}

export function literalTextMatches(source: string, query: string): TextMatch[] {
  if (!query) return [];
  const haystack = source.toLowerCase();
  const needle = query.toLowerCase();
  const matches: TextMatch[] = [];
  let offset = 0;
  while (offset < haystack.length) {
    const start = haystack.indexOf(needle, offset);
    if (start < 0) break;
    matches.push({ start, end: start + needle.length });
    offset = start + needle.length;
  }
  return matches;
}

export function splitMarkdownSectionRanges(source: string): MarkdownSectionRange[] {
  const normalized = source.replace(/\r\n/g, "\n");
  if (normalized.includes("<!-- kokoro-reader-section -->")) {
    const marker = "<!-- kokoro-reader-section -->";
    const ranges: MarkdownSectionRange[] = [];
    let offset = 0;
    for (const part of normalized.split(marker)) {
      const leading = part.search(/\S/u);
      if (leading >= 0) {
        const text = part.trim();
        const start = offset + leading;
        ranges.push({ text, start, end: start + text.length });
      }
      offset += part.length + marker.length;
    }
    return ranges;
  }
  const lines = source.replace(/\r\n/g, "\n").split("\n");
  const sections: MarkdownSectionRange[] = [];
  let current: string[] = [];
  let currentStart = 0;
  let offset = 0;
  let fenced = false;
  for (const line of lines) {
    const trimmed = line.trim();
    if (trimmed.startsWith("```") || trimmed.startsWith("~~~")) fenced = !fenced;
    if (!fenced && trimmed === "" && current.length > 0) {
      const text = current.join("\n").trim();
      if (text) sections.push({ text, start: currentStart, end: currentStart + text.length });
      current = [];
      currentStart = offset + line.length + 1;
    } else {
      current.push(line);
    }
    offset += line.length + 1;
  }
  const text = current.join("\n").trim();
  if (text) sections.push({ text, start: currentStart, end: currentStart + text.length });
  return sections;
}

export function splitMarkdownSections(source: string): string[] {
  return splitMarkdownSectionRanges(source).map(({ text }) => text);
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
  return source.replace(/\r\n/g, "\n").split(/\n\s*\n/u).map((paragraph) => paragraph.trim()).filter(Boolean).flatMap((paragraph) => {
    const sentences = splitSentences(paragraph);
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
  });
}

export function speechChunkRanges(source: string): SpeechChunkRange[] {
  const chunks = groupSpeechChunks(source);
  let searchFrom = 0;
  return chunks.map((text) => {
    const exactStart = source.indexOf(text, searchFrom);
    if (exactStart >= 0) {
      searchFrom = exactStart + text.length;
      return { text, start: exactStart, end: searchFrom };
    }
    return { text, start: 0, end: source.length };
  });
}

export function bestMarkdownPassage(source: string, narration: string): TextMatch {
  const blocks = Array.from(source.matchAll(/\S[\s\S]*?(?=\n\s*\n|$)/gu)).map((match) => ({
    start: match.index ?? 0,
    end: (match.index ?? 0) + match[0].length,
    text: match[0],
  }));
  if (!blocks.length) return { start: 0, end: source.length };
  const match = narrationHighlights(blocks.map((block) => deriveSpeechText(block.text)), narration)[0];
  return match ? { start: blocks[match.blockIndex].start, end: blocks[match.blockIndex].end } : { start: 0, end: source.length };
}
