# Speech normalization

Do not pronounce Markdown punctuation, raw URLs, fences, or identifier syntax. Expand identifiers into readable words and use the pronunciation glossary for explicit overrides.

## Identifier Read-Aloud Gate

Keep the exact identifier in the visible card. In narration, convert it to ordinary words before sending:

- camelCase or PascalCase: `groupSpeechChunks` → “group speech chunks”; `TextMatch` → “text match”.
- snake_case or kebab-case: `audio_cache_status` → “audio cache status”; `pre-check` → “pre check”.
- dotted paths and URLs: say only the meaningful name when needed; never read punctuation literally.

The pronunciation glossary overrides these defaults. Before transfer, check every visible code identifier or path that is mentioned aloud. Record a quick visible-to-spoken mapping while drafting, and reject narration that still contains camelCase, snake_case, raw URLs, backticks, or fence markers.

Keep visible names, numbers, and key terms in the narration wherever natural. Explain equations in plain language rather than reading symbols. Preserve numeric values when they determine behavior; use the visible answer for every self-check.

For code ranges, say “Lines 53 through 55” rather than reciting each line number. A range is traceability, not source-code narration: explain the shared behavior in ordinary language.
