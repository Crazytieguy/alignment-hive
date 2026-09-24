// Bun embeds these assets with `with { type: 'text' }`, including compiled binaries.
declare module '*.css' { const text: string; export default text; }
declare module '*review-diff.js' { const text: string; export default text; }
declare module '*review-page.js' { const text: string; export default text; }
