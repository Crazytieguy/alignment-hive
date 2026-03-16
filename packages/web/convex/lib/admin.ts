export function getAdminEmails(): string[] {
  const envValue = process.env.ADMIN_EMAILS ?? "";
  return envValue
    .split("\n")
    .map((e) => e.trim())
    .filter(Boolean);
}
