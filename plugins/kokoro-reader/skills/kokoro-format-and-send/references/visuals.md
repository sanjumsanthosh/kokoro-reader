# Visual cards

Use fenced `text` diagrams only when they clarify a flow, comparison, sequence, ownership boundary, state transition, or spatial relationship. Put the diagram next to its explanation and add a short visible caption that names each meaningful path in reading order.

Use a sequence for request/response timing, a branch for alternatives, a layered map for boundaries and ownership, a comparison for trade-offs, and a timeline for change over time. Keep labels short and introduce them in nearby prose. Do not use a diagram as decoration.

For a code worked example, prefer a compact state strip beside the relevant lines:

```text
Example input ──▶ transformed state ──▶ example output ──▶ next consumer
```

Replace the labels with the actual values or compact names from the same continuing example. The caption must state what changed at each arrow. Use a small table when the state has several fields.
