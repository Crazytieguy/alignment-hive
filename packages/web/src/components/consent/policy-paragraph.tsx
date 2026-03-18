/** Renders policy text with inline code and bold.
 *  CLI commands are delimited by zero-width spaces (\u200B) in the source text.
 *  Bold text is delimited by ** (e.g. **important**). */
export function PolicyParagraph({ text }: { text: string }) {
  // First split on zero-width spaces for code spans, then handle bold within text spans.
  const codeParts = text.split(/\u200B/);
  const nodes: React.ReactNode[] = [];

  for (let i = 0; i < codeParts.length; i++) {
    if (i % 2 === 1) {
      nodes.push(
        <code
          key={`c${i}`}
          className="bg-muted px-1.5 py-0.5 rounded text-[0.85em] font-mono"
        >
          {codeParts[i]}
        </code>,
      );
    } else {
      // Split on **bold** within text spans
      const boldParts = codeParts[i].split(/\*\*([^*]+)\*\*/);
      for (let j = 0; j < boldParts.length; j++) {
        if (j % 2 === 1) {
          nodes.push(<strong key={`b${i}-${j}`}>{boldParts[j]}</strong>);
        } else if (boldParts[j]) {
          nodes.push(<span key={`t${i}-${j}`}>{boldParts[j]}</span>);
        }
      }
    }
  }

  return <p>{nodes}</p>;
}
