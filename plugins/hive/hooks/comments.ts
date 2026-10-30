// An ArtifactComments read cut to its comments and split into threads, with no engine or Node imports: hive-cli cuts
// the reads in transcripts with these, and automatic recall the read Claude just made, so both name a thread alike.

/**
 * The comments of an ArtifactComments result, without the tool's instructions around them: the block between its
 * BEGIN line (a header of instructions) and the END line carrying the same nonce. Viewer text sits on indented lines,
 * so neither line can be forged. Any other result is kept whole.
 */
export function commentsOnly(result: string): string {
  const begins = [...result.matchAll(/^=== BEGIN ARTIFACT COMMENTS (\S+) — .* ===$/gm)]
  if (begins.length !== 1) return result
  const [begin] = begins as [RegExpExecArray]
  const from = begin.index + begin[0].length + 1
  const end = result.indexOf(`\n=== END ARTIFACT COMMENTS ${begin[1]} ===`, from - 1)
  return end < 0 ? result : result.slice(from, end)
}

const THREAD = /^Thread ([0-9a-f]{8})-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/gm

/**
 * The threads of commentsOnly's output, each its `Thread <uuid>` line and the indented lines under it, named by the
 * uuid's first 8 characters. The tool writes its own rows unindented (thread lines, and a size cap's note on threads
 * not listed) and viewer text indented, so viewer text cannot forge either. Undefined, for the read to stay whole, when
 * the comments don't open with a thread or two threads share a name.
 */
export function commentThreads(comments: string): Array<{ id: string; text: string }> | undefined {
  const starts = [...comments.matchAll(THREAD)]
  if (!starts.length || comments.slice(0, starts[0]!.index).trim()) return undefined
  const threads = starts.map((m, i) => {
    const [head, ...lines] = comments.slice(m.index, starts[i + 1]?.index).split('\n')
    const end = lines.findIndex(l => /^\S/.test(l))
    return { id: m[1]!, text: [head, ...(end < 0 ? lines : lines.slice(0, end))].join('\n').trimEnd() }
  })
  return new Set(threads.map(t => t.id)).size === threads.length ? threads : undefined
}
