/** Renders policy text with inline code for CLI commands.
 *  CLI commands are delimited by zero-width spaces (\u200B) in the source text. */
export function PolicyParagraph({ text }: { text: string }) {
  const parts = text.split(/\u200B/);
  if (parts.length <= 1) return <p>{text}</p>;
  return (
    <p>
      {parts.map((part, i) =>
        i % 2 === 1 ? (
          <code
            key={i}
            className="bg-muted px-1.5 py-0.5 rounded text-[0.85em] font-mono"
          >
            {part}
          </code>
        ) : (
          <span key={i}>{part}</span>
        ),
      )}
    </p>
  );
}
