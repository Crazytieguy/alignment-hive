interface AccessListProps {
  accessList: Array<{ name: string | null; email: string }>;
}

export function AccessList({ accessList }: AccessListProps) {
  if (accessList.length === 0) return null;

  return (
    <div className="rounded-lg border px-5 py-4">
      <p className="text-sm font-medium mb-3">
        People with access to shared data
      </p>
      <ul className="space-y-1.5">
        {accessList.map((person, i) => (
          <li
            key={i}
            className="text-sm text-muted-foreground flex items-baseline gap-2"
          >
            <span className="size-1.5 rounded-full bg-primary/40 shrink-0 mt-1.5" />
            {person.name ? (
              <span>
                {person.name}{" "}
                <span className="text-muted-foreground/60">
                  ({person.email})
                </span>
              </span>
            ) : (
              <span>{person.email}</span>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}
