// Keep consistent with ../../../src/components/consent/policy-content.ts —
// changes to access/use terms must be reflected in both files.

export const CURRENT_AGREEMENT_VERSION = "2026-03";

export interface AgreementSection {
  id: string;
  title: string | null;
  paragraphs: string[];
}

export const agreementSections: AgreementSection[] = [
  {
    id: "preamble",
    title: null,
    paragraphs: [
      "By receiving access to Alignment Hive session data, you agree to the following. This agreement binds you as an individual, not an organization.",
    ],
  },
  {
    id: "access-and-use",
    title: "Access and use",
    paragraphs: [
      "You are receiving Claude Code session data shared by AI safety researchers, along with their names and email addresses. Some sessions include linked private repository content.",
      "Your name and email address will be displayed to data contributors so they can see who has access to their data.",
      "Sessions go through automated secret removal before upload, but this isn't perfect. Treat any credentials you encounter as confidential and do not use them.",
      "This data is shared to support AI safety research. You may not sell or commercialize the data, or use it to develop commercial products or services.",
    ],
  },
  {
    id: "quoting-and-crediting",
    title: "Quoting and crediting contributors",
    paragraphs: [
      "Contributors set preferences about being quoted and credited. If a contributor has opted out of verbatim excerpts, do not quote from their sessions. If they have opted out of being named, do not name them. If a contributor has opted in to being credited, crediting them is encouraged but not required.",
      "Make a best effort to anonymize all verbatim excerpts in public-facing outputs (publications, presentations, blog posts, etc.), including removing identifiable project details, file paths, and repository references. Consent preferences are visible alongside the data.",
      "Citing Alignment Hive as a data source is encouraged.",
    ],
  },
  {
    id: "preventing-leaks",
    title: "Preventing leaks",
    paragraphs: [
      "Take reasonable steps to prevent the data from being leaked. Do not use session data as input to AI services that may use it for training or that don't guarantee privacy.",
      "Store the data securely. Do not disclose the data to others. Anyone who needs access must request it from Alignment Hive directly and sign this agreement themselves.",
      "Do not contact contributors using information from the data unless authorized by Alignment Hive.",
    ],
  },
  {
    id: "deletion-requests",
    title: "Deletion requests",
    paragraphs: [
      "Keep track of which data came from which contributor so you can respond to per-user deletion requests. When Alignment Hive notifies you of a deletion request, delete the relevant data promptly and within 30 days, then confirm. This includes copies and backups where feasible. Published research findings and trained model weights that can't practically be undone are excluded, consistent with the best-effort commitment we make to users.",
    ],
  },
  {
    id: "security-breaches",
    title: "Security breaches",
    paragraphs: [
      "If someone gains unauthorized access to data you hold, notify Alignment Hive at yoav.tzfati@gmail.com within 72 hours of discovering the unauthorized access so we can inform affected users.",
    ],
  },
  {
    id: "violations",
    title: "Violations",
    paragraphs: [
      "If you violate this agreement, Alignment Hive may revoke your access and require you to delete all data you hold. You must do so.",
    ],
  },
  {
    id: "role-changes",
    title: "Role changes",
    paragraphs: [
      "If you are no longer using the data for AI safety research, notify Alignment Hive so we can revoke access and you can delete the data.",
    ],
  },
  {
    id: "disclaimer",
    title: "Disclaimer",
    paragraphs: ["The data is provided as-is, without warranty of any kind."],
  },
  {
    id: "changes",
    title: "Changes to this agreement",
    paragraphs: [
      "We may update this agreement. If we do, we'll notify you and ask you to re-confirm. If you don't re-confirm, your access will be revoked.",
      "Your obligations regarding confidentiality, deletion, and breach notification survive termination of this agreement.",
    ],
  },
];

export const agreementFooter =
  "Alignment Hive is operated by Yoav Tzfati. For questions or requests, contact yoav.tzfati@gmail.com.";
