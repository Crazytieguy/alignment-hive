// Keep consistent with ../../../convex/lib/agreement.ts —
// changes to access/use terms must be reflected in both files.

/** Single source of truth for user-facing data sharing policy text.
 *  CLI commands are wrapped in \u200B (zero-width space) delimiters for inline code rendering. */

export type ConsentQuestion =
  | "sessionSharing"
  | "communityFeatures"
  | "publicationExcerpts"
  | "creditByName";

export type SectionContent =
  | { kind: "paragraph"; text: string; policyText?: string }
  | { kind: "question"; id: ConsentQuestion };

export type PolicySectionId =
  | "intro"
  | "controlling"
  | "access"
  | "community-features"
  | "publications";

export interface PolicySection {
  id: PolicySectionId;
  title: string | null;
  content: SectionContent[];
}

export const policySections: PolicySection[] = [
  {
    id: "intro",
    title: null,
    content: [
      { kind: "paragraph", text: "You don't have to share anything to use the platform, but sharing your Claude Code sessions helps the AI safety research community: measuring uplift from coding agents, constructing realistic evaluations from real-world usage, and sharing skills, tools, and knowledge across the community. You can review and change these preferences at any time at alignment-hive.com/consent." },
    ],
  },
  {
    id: "controlling",
    title: "Controlling what you share",
    content: [
      { kind: "paragraph", text: "Sharing is opt-in per project and can be stopped at any time, though previously uploaded data is not automatically deleted. You choose which projects to share via the install script, the CLI (\u200Bhive consent enable\u200B / \u200Bhive consent disable\u200B), or /hive:align inside Claude Code." },
      { kind: "paragraph", text: "Before upload, sessions are cleaned of credentials to the best of our ability, as well as binary content such as images and PDFs. When you enable sharing for a project, existing and new sessions each go through a 24-hour review period, followed by a reminder and 10-minute window at the start of your next Claude Code session. You can exclude or delay any session during either window using \u200Bhive upload review\u200B." },
    ],
  },
  {
    id: "access",
    title: "Access and use",
    content: [
      { kind: "paragraph", text: "**Your data will not be used for any commercial purpose, including sale to AI labs for training.** Your data, name, and email are shared with a curated group of AI safety researchers that may change over time. We ask all parties with access to take reasonable steps to prevent the data from being leaked, for example by not feeding it into AI services that may train on inputs." },
      { kind: "paragraph", text: "Data is retained for as long as alignment-hive operates, unless you request deletion at yoav.tzfati@gmail.com. We'll delete what we control and require others with access to maintain provenance and do the same, but we cannot guarantee that no copies exist. If your data is compromised in a security breach, we will notify you as soon as possible." },
      { kind: "paragraph", text: "By sharing sessions, you grant alignment-hive a non-exclusive, royalty-free license to use, store, and share your session data for the purposes described in this policy. You represent that you have the right to share this data, and that doing so does not violate any agreements you are bound by (e.g. employer policies or NDAs)." },
      { kind: "question", id: "sessionSharing" },
    ],
  },
  {
    id: "community-features",
    title: "Community features",
    content: [
      { kind: "paragraph", text: "We're building tools to aggregate useful knowledge from sessions, such as skills and tools that prove useful. We'll only redistribute to other vetted members of the community, and make a best effort to only include impersonal or redacted content.", policyText: "We're building tools to aggregate useful knowledge from sessions, such as skills and tools that prove useful. We'll only redistribute to other vetted members of the community, and make a best effort to only include impersonal or redacted content. You can choose whether to allow your sessions to be used for this." },
      { kind: "question", id: "communityFeatures" },
    ],
  },
  {
    id: "publications",
    title: "Research publications",
    content: [
      { kind: "paragraph", text: "Your sessions may also be used in published AI safety research, subject to your preferences below.", policyText: "Your sessions may also be used in published AI safety research. You can choose whether to allow verbatim session excerpts and whether to be credited by name." },
      { kind: "question", id: "publicationExcerpts" },
      { kind: "question", id: "creditByName" },
    ],
  },
];

export const policyFooter =
  "alignment-hive is operated by Yoav Tzfati. For questions or requests, contact yoav.tzfati@gmail.com.";

/** Short explanation shown on the projects management page. */
export const projectSharingNote =
  "Disabling sharing for a project stops future sessions from being uploaded. Previously uploaded data is not automatically deleted \u2014 contact yoav.tzfati@gmail.com to request deletion.";

/** Explanation of code context for the GitHub App install step. */
export const codeContextExplanation =
  "For private repos, you can grant read access via our GitHub App so researchers can see the code referenced in your sessions. You choose which repositories, and you can revoke access at any time from GitHub.";

export interface QuestionConfig {
  id: ConsentQuestion;
  label: string | null;
  yesLabel: string;
  noLabel: string;
}

export const questionConfigs: QuestionConfig[] = [
  {
    id: "sessionSharing",
    label: null,
    yesLabel: "Enable sharing",
    noLabel: "Share nothing",
  },
  {
    id: "communityFeatures",
    label: "Allow your sessions to be used for community features?",
    yesLabel: "Allow",
    noLabel: "Don't allow",
  },
  {
    id: "publicationExcerpts",
    label:
      "Allow verbatim session excerpts to appear in published research?",
    yesLabel: "Allow verbatim excerpts",
    noLabel: "Don't allow",
  },
  {
    id: "creditByName",
    label: "If your data is used in research publications, would you like to be credited by name?",
    yesLabel: "Credit me",
    noLabel: "Stay anonymous",
  },
];
