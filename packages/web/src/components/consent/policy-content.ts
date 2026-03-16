/** Single source of truth for user-facing data sharing policy text.
 *  Each section contains the policy text, broken into renderable chunks.
 *
 *  Section order: intro → what-gets-shared → access → controlling (with sharing Q) →
 *  community-features → publications → footer.
 *
 *  CLI commands are wrapped in \u200B (zero-width space) delimiters for inline code rendering. */

export interface PolicySection {
  id: string;
  title: string | null;
  paragraphs: string[];
}

export const policySections: PolicySection[] = [
  {
    id: "intro",
    title: null,
    paragraphs: [
      "This page lets you set your data sharing preferences for alignment-hive. You don't have to share anything to use the platform, but sharing session data helps the AI safety research community. You can review and change these preferences at any time at alignment-hive.com/consent, changes apply going forward and do not affect previously shared data.",
      "This data has many potential applications for AI safety research: measuring uplift from coding agents, constructing realistic evaluations from real-world usage, and sharing skills, tools, and knowledge across the community.",
    ],
  },
  {
    id: "what-gets-shared",
    title: "What gets shared",
    paragraphs: [
      "Your Claude Code sessions, with certain content excluded: images, PDFs, and other non-text content are stripped out. Sessions go through automated secret removal before upload. This catches the vast majority of secrets, but we can't guarantee it catches everything. If you know a session contains sensitive credentials, it's worth reviewing or excluding it.",
      "Keep in mind that your sessions may contain other people's information, like pasted messages or collaborator names. If a session contains sensitive third-party content, consider excluding it.",
      "By sharing sessions, you grant alignment-hive a non-exclusive, royalty-free license to use, store, and share your session data for the purposes described in this policy. You represent that you have the right to share this data, and that doing so does not violate any agreements you are bound by (e.g. employer policies or NDAs).",
      "We also use the commit hashes in your sessions to correlate them with the state of your codebase, in order to reproduce the environment the session ran in. For private repos, this requires granting us read access, which is a separate per-project opt-in.",
    ],
  },
  {
    id: "access",
    title: "Access and use",
    paragraphs: [
      "Your data is shared with a curated group of AI safety researchers and organizations. You can visit this page at any time to see the current list.",
      "We will not monetize your data or sell it to AI labs for training or any other commercial purpose. We ask all parties with access to take reasonable steps to prevent the data from being leaked, for example by not using it as input to AI services that may use the data for training or that don't guarantee privacy.",
      "Data is retained for as long as alignment-hive operates, unless you request deletion. To request deletion, contact yoav.tzfati@gmail.com. We plan to add self-serve deletion in the future. We'll delete what we control and notify others with access to do the same. This is best-effort: all parties with access are required to maintain provenance and honor deletion requests, but we cannot guarantee that no copies exist.",
      "If your data is compromised in a security breach, we will notify you as soon as possible.",
    ],
  },
  {
    id: "controlling",
    title: "Controlling what you share",
    paragraphs: [
      "Consenting here does not mean your data is uploaded immediately. Sharing is opt-in per project: you choose which projects to share using the CLI (\u200Bhive consent enable\u200B), the install script, or by running /hive:align inside Claude Code. When you enable sharing for a project, all existing sessions in that project enter a 24-hour review period. New sessions enter the review period as they're created.",
      "After the review period, you'll see a reminder at the start of your next Claude Code session and have a 10-minute window before the upload begins. You can exclude any session during either window, or exclude all pending sessions at once. If you know a session contains sensitive credentials, you can review and exclude it during this period. If you need more time, you can delay the upload.",
      "You can stop sharing a project at any time using the CLI (\u200Bhive consent disable\u200B) or by running /hive:align. Previously uploaded data is not automatically deleted when you stop sharing.",
    ],
  },
  {
    id: "community-features",
    title: "Community features",
    paragraphs: [
      "As an additional opt-in, your sessions can also be used for community features. We're building tools that let researchers' Claude agents learn from each other's sessions, for example discovering useful skills or tools that other researchers have developed. If you opt in, your sessions may be returned in processed form through an API available to other alignment-hive users. We will make a best effort to redact personal data or exclude sessions with personal content before they are surfaced through these features. Access to alignment-hive is invite-only and limited to trusted AI safety researchers.",
    ],
  },
  {
    id: "publications",
    title: "Research publications",
    paragraphs: [
      "As a further opt-in, AI safety researchers may include verbatim excerpts from your sessions as examples in published papers. This data is not monetized.",
      "Separately, you can indicate whether you'd like to be credited by name when your data is used in research publications. This applies whether or not you allow verbatim excerpts — your data may inform research even without being quoted directly. If you choose not to be credited, your name will not be included in any publications that use your data.",
    ],
  },
];

export const policyFooter =
  "alignment-hive is operated by Yoav Tzfati. For questions or requests, contact yoav.tzfati@gmail.com.";

export type ConsentQuestion =
  | "sessionSharing"
  | "communityFeatures"
  | "publicationExcerpts"
  | "creditByName";

export const sectionQuestions: Record<string, ConsentQuestion | undefined> = {
  controlling: "sessionSharing",
  "community-features": "communityFeatures",
  publications: "publicationExcerpts",
};

/** Credit question appears after publicationExcerpts, regardless of its answer. */
export const creditQuestionAfter = "publications";

export interface QuestionConfig {
  id: ConsentQuestion;
  label: string;
  yesLabel: string;
  noLabel: string;
  gatesRest?: boolean;
}

export const questionConfigs: QuestionConfig[] = [
  {
    id: "sessionSharing",
    label: "Share your sessions with the alignment research community?",
    yesLabel: "Share my sessions",
    noLabel: "Don't share",
    gatesRest: true,
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
    label: "How should you be credited in research publications?",
    yesLabel: "Credit me by name",
    noLabel: "Keep anonymous",
  },
];
