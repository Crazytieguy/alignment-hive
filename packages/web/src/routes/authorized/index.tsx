import { createFileRoute, Link } from "@tanstack/react-router";

export const Route = createFileRoute("/authorized/")({
  component: AuthorizedIndex,
});

const cards = [
  {
    to: "/authorized/sessions" as const,
    title: "Sessions",
    description: "Browse shared session data from alignment researchers.",
  },
  {
    to: "/authorized/users" as const,
    title: "Users",
    description: "View contributors who have shared session data.",
  },
  {
    to: "/authorized/data-access" as const,
    title: "Data Access",
    description: "CLI setup, HTTP API, and API key management.",
  },
  {
    to: "/authorized/agreement" as const,
    title: "Data Accessor Agreement",
    description: "View the agreement governing your access to session data.",
  },
];

function AuthorizedIndex() {
  return (
    <div className="max-w-2xl mx-auto">
      <h1 className="text-2xl font-semibold mb-2 tracking-tight">
        Data Access
      </h1>
      <p className="text-sm text-muted-foreground mb-8">
        Access shared session data from alignment researchers.
      </p>
      <div className="grid gap-4">
        {cards.map((card) => (
          <Link
            key={card.to}
            to={card.to}
            className="block rounded-lg border border-border p-4 hover:bg-muted/50 transition-colors"
          >
            <h2 className="font-medium text-foreground">{card.title}</h2>
            <p className="text-sm text-muted-foreground mt-1">
              {card.description}
            </p>
          </Link>
        ))}
      </div>
    </div>
  );
}
