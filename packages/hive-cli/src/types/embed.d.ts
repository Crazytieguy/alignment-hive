// Bun's `import p from './x.bundle' with { type: 'file' }` embeds the file and yields its path.
declare module '*.bundle' {
  const path: string;
  export default path;
}
