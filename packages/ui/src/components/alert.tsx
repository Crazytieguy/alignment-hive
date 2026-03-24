import { cn } from "../lib/utils";

const variantStyles = {
  error:
    "border-red-500/30 bg-red-50 text-red-800 dark:bg-red-900/20 dark:text-red-200",
  warning:
    "border-yellow-500/30 bg-yellow-50 text-yellow-800 dark:bg-yellow-900/20 dark:text-yellow-200",
} as const;

interface AlertProps {
  variant: keyof typeof variantStyles;
  children: React.ReactNode;
  className?: string;
}

export function Alert({ variant, children, className }: AlertProps) {
  return (
    <div
      className={cn(
        "rounded-lg border px-4 py-2 text-sm",
        variantStyles[variant],
        className,
      )}
    >
      {children}
    </div>
  );
}
