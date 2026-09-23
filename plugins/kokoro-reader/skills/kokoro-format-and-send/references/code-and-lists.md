# Code and lists

For code, keep the file and real line range when available. Narrate adjacent meaningful lines as one semantic group when they form one condition, transformation, or outcome. State an inclusive range, such as “Lines 53 through 55,” then explain the group’s input or state, operation, output, and next consumer. Mention an individual line only when it introduces a distinct branch, call, return value, or security decision. Do not narrate braces, declaration-only lines, imports, or formatting-only lines independently. Follow a supplied callee immediately after its call site.

For executable code, parsing, conditions, data flow, and calls, use a continuing worked example. Choose the user's real input first, then a repository test, fixture, caller, or bug report, then a clearly labeled minimal synthetic input. Keep it unchanged through every connected card.

Use this visible order:

1. `**Example input:**` with the exact concrete value.
2. Exact code and real line numbers.
3. One visible explanation for every meaningful line or coupled group, including the example value before and after that operation. The narration must name the matching individual line or inclusive group range.
4. A compact state strip or table: input → intermediate state → output.
5. `**Example output:**` and the named next consumer.

For a branch, show the evaluated condition and selected path. For a call, show arguments entering the callee and the returned value returning to its consumer. Label unknown runtime values as unknown rather than inventing them. Do not create artificial examples for imports, declarations, schemas, or static metadata.

For a long list, make each actionable or distinct item its own teaching card. Bundle only genuinely repetitive items under one visible shared label, and begin the matching narration with that same label.
