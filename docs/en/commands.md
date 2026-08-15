# Commands

The everyday CLI, for when you want to store and search memory by hand instead
of relying on autosave.

## Store

```bash
fuckmemory remember "deploys go out through fly.io"   # a plain note
fuckmemory remember "we prefer pnpm here" --kind preference
fuckmemory remember "the api talks to postgres" \
  --src api --rel talks_to --dst postgres          # a structured fact
fuckmemory remember "no emoji in commits" --scope global
```

`--file PATH[:FROM-TO]` attaches the file a memory was learned against, so a
recall can point straight at the source:

```bash
fuckmemory remember "the build runs on node 22" --file Makefile:1-6
```

## Search

```bash
fuckmemory recall "how do we deploy"        # what you believe now
fuckmemory recall "deploy" --raw             # include the raw original notes
fuckmemory recall "pnpm" --as-of 2026-03-01  # what you believed then
fuckmemory recall "deploy" --limit 5         # fewer, higher-ranked hits
fuckmemory recall "deploy" --json            # machine-readable output
```

Recall reads three independent retrievers — BM25, static embeddings, and the
graph of entities — fuses their rankings, drops near-duplicates, and hard-caps
the output at a token budget so it never floods a context window.

## Retract and correct

```bash
fuckmemory forget 42              # soft: stops being live, stays in timeline
fuckmemory forget --query "the replica lags" 
fuckmemory forget 42 --hard       # delete permanently (for secrets)
fuckmemory timeline fly.io        # how that entity's story changed over time
```

Soft forget is the default on purpose: a fact is never "deleted", it is closed
out, so `--as-of` can still answer what you believed before you changed your
mind.

## Debug a ranking

```bash
fuckmemory explain "deploy"       # per-retriever breakdown
```

`explain` shows each retriever's raw view, including the cosine of every fact
and whether it cleared the relevance floor. Use it when a recall surprises you,
or after switching embedding models.

Next: [resume interrupted work](task.md).
