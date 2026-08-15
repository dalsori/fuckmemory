# Maintenance

A memory store that is never consolidated decays into an append-only log that
poisons retrieval. These commands keep it healthy.

## Consolidate and prune

```bash
fuckmemory consolidate              # merge duplicates, close contradictions
fuckmemory prune --days 90          # drop retracted, never-read facts
fuckmemory consolidate && fuckmemory prune --days 90   # the cron pair
```

With autosave on, the session-end hook consolidates for you at the one moment
nobody is waiting on it. Otherwise run the pair from a cron.

`prune` is conservative: anything with a hit is kept, because something read it
once. Use `--dry-run` to see what it would remove first.

## Doctor

```bash
fuckmemory doctor              # paths, schema, model, cache, registrations
fuckmemory doctor --fix        # repair what the check finds, automatically
```

`doctor` verifies the store schema, the embedding model and its cache, and that
your agents are still wired. `--fix` rebuilds missing FTS indexes, fetches the
model, re-embeds if the model changed, wires detected agents, and consolidates.

## Reindex

Switch embedding models? Vectors from two models are not comparable.

```bash
fuckmemory reindex              # re-embed every fact and episode
```

`doctor --fix` also does this automatically when it detects a model change.

## Bench

Reproducible latency numbers on a throwaway store — your real database is never
touched:

```bash
fuckmemory bench                # write/recall medians, hot and cold
./bench.sh                      # same, plus a markdown table + ASCII chart
```

## Export and import

```bash
fuckmemory export                     # dump the current scope as JSON
fuckmemory export --scope global      # dump a named scope
fuckmemory import dump.json           # load it back
```

Export/import is a round trip: episodes, their file references and fact
provenance are all preserved, and re-importing is idempotent.

## Stats

```bash
fuckmemory stats               # scopes, episodes, facts, entities, size
fuckmemory scopes              # list the memory scopes
```

Next: [troubleshooting](troubleshooting.md).
