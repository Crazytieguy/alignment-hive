import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@alignment-hive/ui";

interface AccessListProps {
  accessList: Array<{ name: string | null; email: string }>;
}

export function AccessList({ accessList }: AccessListProps) {
  if (accessList.length === 0) return null;

  return (
    <div className="rounded-lg border px-5 py-4">
      <p className="text-sm font-medium mb-3">
        Researchers with access
      </p>
      <AccessListItems accessList={accessList} />
    </div>
  );
}

export function CollapsibleAccessList({
  accessList,
  className,
}: AccessListProps & { className?: string }) {
  if (accessList.length === 0) return null;

  return (
    <Collapsible className={className}>
      <CollapsibleTrigger className="text-sm font-medium cursor-pointer text-muted-foreground hover:text-foreground transition-colors flex items-center gap-1.5">
        <svg
          width="12"
          height="12"
          viewBox="0 0 12 12"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          className="transition-transform duration-200 [[data-state=open]_&]:rotate-90"
        >
          <path d="M4.5 2.5l3.5 3.5-3.5 3.5" />
        </svg>
        Researchers with access ({accessList.length})
      </CollapsibleTrigger>
      <CollapsibleContent className="mt-3">
        <AccessListItems accessList={accessList} />
      </CollapsibleContent>
    </Collapsible>
  );
}

function AccessListItems({ accessList }: AccessListProps) {
  return (
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
  );
}
