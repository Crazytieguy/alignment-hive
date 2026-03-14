// Type declaration for Bun's file embedding via `import ... with { type: "file" }`.
// The import resolves to a path string at compile time.
declare module '*.bundle' {
  const path: string;
  export default path;
}
