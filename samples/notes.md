---
title: Reading notes
tags: [graphs, icl]
---

# Reading notes on in-context learning

Large language models can solve a new task after seeing a few examples in the prompt. This note collects open questions about whether the same idea transfers to graph neural networks.

## Questions

Does the prompt need to be a graph at all, or would a flat list of labelled nodes suffice? A graph lets the model use its message-passing machinery, which seems important.

How many examples per class are needed before accuracy saturates? Early results suggest that three to five are enough on citation graphs.

```python
def build_prompt(graph, examples):
    # Connect each example node to a virtual label node.
    for node, label in examples:
        graph.add_edge(node, f"label:{label}")
    return graph
```

---

## Next steps

Run the ablation on the number of examples, then write up the method section.
