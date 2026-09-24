@README.md

`hive local` reads transcripts only through the `session-data` parser and its `noise.ts` rules, and resolves locators only through `src/lib/locators.ts`; never add a second parser or resolver. Local commands must not persist registry changes.
