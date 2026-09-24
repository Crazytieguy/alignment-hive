// The official standalone ESM bundle exposes the same API as the Node entry.
// @types/css-tree declares only the package root; this adds types, not runtime code.
declare module 'css-tree/dist/csstree.esm' { export * from 'css-tree'; }
