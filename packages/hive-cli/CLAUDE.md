@README.md

`hive local` reads transcripts only through the `session-data` parser and its `noise.ts` rules, and resolves locators only through `src/lib/locators.ts`; never add a second parser or resolver. Local commands must not persist registry changes.

`hive local` reads transcript files and directories through `src/lib/file-memo.ts`, which `hive local-server` keeps between requests: a value computed there depends only on the bytes it was given, and is never mutated.
