// Rule test cases ported from gitleaks: https://github.com/gitleaks/gitleaks
import { describe, expect, test } from 'bun:test';
import { detectSecrets } from '../lib/sanitize';

const RSA_KEY = `-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyf8DqJfIKWNNaLHN9qZjHPQzYpZmL
klmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/ABCDEFGH
-----END RSA PRIVATE KEY-----`;
const PGP_KEY = `-----BEGIN PGP PRIVATE KEY BLOCK-----
lQPGBGRnZ2EBCADQjsT3n6jj4dJFVFbMaZNe9p4Ohfe3kTPJyiLZJR5Kj9mK8sd7
-----END PGP PRIVATE KEY BLOCK-----`;

/** Samples the named rule must fire on (`detect`) and must not fire on (`reject`). */
const CASES: Record<string, { detect: Array<string>; reject?: Array<string>; viaContext?: true }> = {
  'anthropic-api-key': {
    detect: [
      'sk-ant-api03-abc123xyz-456def789ghij-klmnopqrstuvwx-3456yza789bcde-1234fghijklmnopby56aaaogaopaaaabc123xyzAA',
    ],
    reject: [
      'sk-ant-api03-abc123xyz-456de-klMnopqrstuvwx-3456yza789bcde-1234fghijklmnopAA', // too short
      'sk-ant-api03-abc123xyz-456def789ghij-klmnopqrstuvwx-3456yza789bcde-1234fghijklmnopby56aaaogaopaaaabc123xyzBB', // wrong suffix
    ],
  },
  'openai-api-key': {
    detect: [
      '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',
      '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',
      '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',
    ],
  },
  'huggingface-access-token': {
    detect: ['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', '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'],
    reject: ['hf_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'], // low entropy
  },
  'aws-access-token': {
    detect: ['AKIALALEMEL33243OLIB', 'AKIAIOSFODNN7EXAMPLE', 'ASIAJLVYNHUWCPKOPSYQ'],
    reject: ['AKIAXXXXXXXXXXXXXXXX'], // low entropy
  },
  'azure-ad-client-secret': {
    detect: ['7Xp8Q~NYxF.xGwRrghPJV3bWOTevGk3~uEHsGab8', 'Xxz8Q~hXLPiLERKqRLlFnJu.M2CjqZvbqePR_a0N'],
  },
  'github-pat': {
    detect: ['ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs'],
    reject: ['ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'], // low entropy
  },
  'gitlab-pat': {
    detect: ['glpat-1a2B3c4D5e6F7g8H9i0J'],
    reject: ['glpat-xxxxxxxxxxxxxxxxxxxx'], // low entropy
  },
  'npm-access-token': { detect: ['npm_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs'] },
  'slack-bot-token': {
    detect: [
      'xoxb-123456789012-1234567890123-1a2B3c4D5e6F7g8H9i0J1k2L',
      'xoxb-17653672481-19874698323-pdFZKVeTuE8sk7oOcBrzbqgy',
    ],
  },
  'slack-webhook-url': {
    detect: [
      'https://hooks.slack.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX',
      'https://hooks.slack.com/services/T06Q5QMJD/A08GA3P0Y00/4tU2qFZe0NbAhGSJC4ZXoPcZ',
    ],
  },
  'jwt': {
    detect: [
      'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiYWRtaW4iOnRydWV9.TJVA95OrM7E2cBab30RMHrHDcEfxjoYZgeFONFh7HgQ',
      'eyJhbGciOiJIUzUxMiIsInR5cCI6IkpXVCJ9.eyJhY2Nlc3NLZXkiOiJRMzFDVlMxUFNDSjRPVEsyWVZFTSIsImF0X2hhc2giOiI4amItZFE2OXRtZEVueUZaMUttNWhnIn0.nrbzIJz99Om7TvJ04jnSTmhvlM7aR9hMM1Aqjp2ONJ1UKYCvegBLrTu6cYR968_OpmnAGJ8vkd7sIjUjtR4zbw',
    ],
  },
  'generic-api-key': {
    detect: [
      'api_key = "6fe4476ee5a1832882e326b506d14126"',
      'SECRET_KEY: a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6',
      'token = "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"',
    ],
    reject: [
      'commit_hash = a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6', // no trigger keyword
      'uuid: 550e8400-e29b-41d4-a716-446655440000', // UUID format
    ],
  },
  'stripe-access-token': {
    detect: ['sk_live_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P', 'rk_live_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P'],
  },
  'sendgrid-api-token': { detect: ['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'] },
  // The keyword context is claimed by generic-api-key first and overlapping spans dedupe to one.
  'mailchimp-api-key': { detect: ['mailchimp_api_key = b5b9f8e50c640da28993e8b6a48e3e53-us18'], viaContext: true },
  '1password-secret-key': {
    detect: ['A3-ASWWYB-798JRYLJVD4-23DC2-86TVM-H43EB', 'A3-ASWWYB-798JRY-LJVD4-23DC2-86TVM-H43EB'],
    reject: ['A3-XXXXXX-XXXXXXXXXXX-XXXXX-XXXXX-XXXXX'], // low entropy placeholder
  },
  'gcp-api-key': {
    detect: ['AIzaSyNHxIf32IQ1a1yjl3ZJIqKZqzLAK1XhDk-'],
    reject: ['apiKey: "AIzaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"'], // insufficient entropy
  },
  'kubernetes-secret-yaml': {
    // (?s:.) is ported to [\s\S], so both orderings of kind/data match. gitleaks rejects empty
    // values and template variables; the JS port is more aggressive, and over-redaction is the
    // safe direction here.
    detect: [
      'apiVersion: v1\nkind: Secret\nmetadata:\n  name: my-secret\ndata:\n  password: c2VjcmV0cGFzc3dvcmQ=',
      'apiVersion: v1\nmetadata:\n  name: my-secret\ndata:\n  password: c2VjcmV0cGFzc3dvcmQ=\nkind: Secret',
    ],
  },
  'private-key': {
    detect: [RSA_KEY, PGP_KEY],
    reject: ['-----BEGIN PRIVATE KEY-----\nanything\n-----END PRIVATE KEY-----'], // minimal content
  },
};

const firedRule = (ruleId: string, s: string): boolean => detectSecrets(s).some((m) => m.ruleId === ruleId);

for (const [ruleId, { detect, reject = [], viaContext }] of Object.entries(CASES)) {
  describe(ruleId, () => {
    test.each(detect)('detects %s', (s) =>
      expect(viaContext ? detectSecrets(s).length > 0 : firedRule(ruleId, s)).toBe(true),
    );
    if (reject.length > 0) test.each(reject)('rejects %s', (s) => expect(firedRule(ruleId, s)).toBe(false));
  });
}
