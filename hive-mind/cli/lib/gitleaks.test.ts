/**
 * Tests ported from gitleaks to validate our secret detection rules.
 * Source: https://github.com/gitleaks/gitleaks
 *
 * These tests validate that:
 * 1. True positives are detected (secrets that SHOULD be caught)
 * 2. False positives are NOT detected (non-secrets that look similar)
 */
import { describe, expect, test } from 'bun:test';
import { detectSecrets } from './sanitize';

describe('AI APIs', () => {
  describe('anthropic-api-key', () => {
    test('detects valid secrets', () => {
      const secrets = [
        'sk-ant-api03-abc123xyz-456def789ghij-klmnopqrstuvwx-3456yza789bcde-1234fghijklmnopby56aaaogaopaaaabc123xyzAA',
      ];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        // Too short
        'sk-ant-api03-abc123xyz-456de-klMnopqrstuvwx-3456yza789bcde-1234fghijklmnopAA',
        // Wrong suffix
        'sk-ant-api03-abc123xyz-456def789ghij-klmnopqrstuvwx-3456yza789bcde-1234fghijklmnopby56aaaogaopaaaabc123xyzBB',
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === 'anthropic-api-key')).toBe(false);
      }
    });
  });

  describe('openai-api-key', () => {
    test('detects valid secrets', () => {
      const secrets = [
        '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',
        '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',
        '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',
      ];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });

  describe('huggingface-access-token', () => {
    test('detects valid secrets', () => {
      const secrets = ['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', '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'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        'hf_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx', // low entropy
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === 'huggingface-access-token')).toBe(false);
      }
    });
  });
});

describe('Cloud Providers', () => {
  describe('aws-access-token', () => {
    test('detects valid secrets', () => {
      const secrets = ['AKIALALEMEL33243OLIB', 'AKIAIOSFODNN7EXAMPLE', 'ASIAJLVYNHUWCPKOPSYQ'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        'AKIAXXXXXXXXXXXXXXXX', // low entropy (repeated X)
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === 'aws-access-token')).toBe(false);
      }
    });
  });

  describe('azure-ad-client-secret', () => {
    test('detects valid secrets', () => {
      const secrets = ['7Xp8Q~NYxF.xGwRrghPJV3bWOTevGk3~uEHsGab8', 'Xxz8Q~hXLPiLERKqRLlFnJu.M2CjqZvbqePR_a0N'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });
});

describe('Code Hosting', () => {
  describe('github-pat', () => {
    test('detects valid secrets', () => {
      // High-entropy GitHub PAT
      const secrets = ['ghp_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        'ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx', // low entropy
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === 'github-pat')).toBe(false);
      }
    });
  });

  describe('gitlab-pat', () => {
    test('detects valid secrets', () => {
      const secrets = ['glpat-1a2B3c4D5e6F7g8H9i0J'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        'glpat-xxxxxxxxxxxxxxxxxxxx', // low entropy
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === 'gitlab-pat')).toBe(false);
      }
    });
  });

  describe('npm-access-token', () => {
    test('detects valid secrets', () => {
      const secrets = ['npm_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P7qRs'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });
});

describe('Communication', () => {
  describe('slack-bot-token', () => {
    test('detects valid secrets', () => {
      const secrets = [
        'xoxb-123456789012-1234567890123-1a2B3c4D5e6F7g8H9i0J1k2L',
        'xoxb-17653672481-19874698323-pdFZKVeTuE8sk7oOcBrzbqgy',
      ];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });

  describe('slack-webhook-url', () => {
    test('detects valid secrets', () => {
      const secrets = [
        'https://hooks.slack.com/services/T00000000/B00000000/XXXXXXXXXXXXXXXXXXXXXXXX',
        'https://hooks.slack.com/services/T06Q5QMJD/A08GA3P0Y00/4tU2qFZe0NbAhGSJC4ZXoPcZ',
      ];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });
});

describe('Authentication', () => {
  describe('jwt', () => {
    test('detects valid secrets', () => {
      const secrets = [
        'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiYWRtaW4iOnRydWV9.TJVA95OrM7E2cBab30RMHrHDcEfxjoYZgeFONFh7HgQ',
        'eyJhbGciOiJIUzUxMiIsInR5cCI6IkpXVCJ9.eyJhY2Nlc3NLZXkiOiJRMzFDVlMxUFNDSjRPVEsyWVZFTSIsImF0X2hhc2giOiI4amItZFE2OXRtZEVueUZaMUttNWhnIn0.nrbzIJz99Om7TvJ04jnSTmhvlM7aR9hMM1Aqjp2ONJ1UKYCvegBLrTu6cYR968_OpmnAGJ8vkd7sIjUjtR4zbw',
      ];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });

  describe('generic-api-key', () => {
    test('detects secrets with context', () => {
      const secrets = [
        'api_key = "6fe4476ee5a1832882e326b506d14126"',
        'SECRET_KEY: a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6',
        'token = "a1b2c3d4e5f6g7h8i9j0k1l2m3n4o5p6"',
      ];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        'commit_hash = a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6', // no trigger keyword
        'uuid: 550e8400-e29b-41d4-a716-446655440000', // UUID format
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === 'generic-api-key')).toBe(false);
      }
    });
  });
});

describe('Other Services', () => {
  describe('stripe-access-token', () => {
    test('detects valid secrets', () => {
      const secrets = ['sk_live_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P', 'rk_live_1a2B3c4D5e6F7g8H9i0J1k2L3m4N5o6P'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });

  describe('sendgrid-api-token', () => {
    test('detects valid secrets', () => {
      // SendGrid tokens are SG. + 66 chars (format: SG.xxxx.yyyy)
      const secrets = ['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'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });

  describe('mailchimp-api-key', () => {
    test('detects valid secrets with context', () => {
      const secrets = ['mailchimp_api_key = b5b9f8e50c640da28993e8b6a48e3e53-us18'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });
  });

  describe('1password-secret-key', () => {
    test('detects valid secrets', () => {
      // From gitleaks whitepaper examples
      const secrets = ['A3-ASWWYB-798JRYLJVD4-23DC2-86TVM-H43EB', 'A3-ASWWYB-798JRY-LJVD4-23DC2-86TVM-H43EB'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        'A3-XXXXXX-XXXXXXXXXXX-XXXXX-XXXXX-XXXXX', // low entropy placeholder
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === '1password-secret-key')).toBe(false);
      }
    });
  });

  describe('google-api-key', () => {
    test('detects valid secrets', () => {
      // From gitleaks test cases
      const secrets = ['AIzaSyNHxIf32IQ1a1yjl3ZJIqKZqzLAK1XhDk-'];
      for (const secret of secrets) {
        expect(detectSecrets(secret).length).toBeGreaterThan(0);
      }
    });

    test('rejects false positives', () => {
      const notSecrets = [
        // Insufficient entropy (all similar characters)
        'apiKey: "AIzaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"',
      ];
      for (const s of notSecrets) {
        const matches = detectSecrets(s);
        expect(matches.some((m) => m.ruleId === 'gcp-api-key')).toBe(false);
      }
    });
  });
});

// Tests for regex conversion edge cases (ported from gitleaks)
describe('Regex Conversion', () => {
  describe('multiline patterns ((?s:.) converted to [\\s\\S])', () => {
    test('kubernetes-secret-yaml detects secrets with kind before data', () => {
      // From gitleaks: kind: Secret before data section
      const k8sSecret = `apiVersion: v1
kind: Secret
metadata:
  name: my-secret
data:
  password: c2VjcmV0cGFzc3dvcmQ=`;
      const matches = detectSecrets(k8sSecret);
      expect(matches.some((m) => m.ruleId === 'kubernetes-secret-yaml')).toBe(true);
    });

    test('kubernetes-secret-yaml detects secrets with data before kind', () => {
      // From gitleaks: data section before kind: Secret (alternate ordering)
      const k8sSecret = `apiVersion: v1
metadata:
  name: my-secret
data:
  password: c2VjcmV0cGFzc3dvcmQ=
kind: Secret`;
      const matches = detectSecrets(k8sSecret);
      expect(matches.some((m) => m.ruleId === 'kubernetes-secret-yaml')).toBe(true);
    });

    // Note: gitleaks rejects empty values and template variables, but our
    // JS regex conversion is more aggressive (see secret-rules.ts header).
    // This is acceptable: false positives (over-redaction) are safer than
    // false negatives (missing secrets) for our use case.
  });

  describe('private-key detection', () => {
    test('detects RSA private key', () => {
      // From gitleaks: standard RSA private key format
      const privateKey = `-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA0Z3VS5JJcds3xfn/ygWyf8DqJfIKWNNaLHN9qZjHPQzYpZmL
klmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/ABCDEFGH
-----END RSA PRIVATE KEY-----`;
      const matches = detectSecrets(privateKey);
      expect(matches.some((m) => m.ruleId === 'private-key')).toBe(true);
    });

    test('detects PGP private key', () => {
      // From gitleaks: PGP private key block
      const privateKey = `-----BEGIN PGP PRIVATE KEY BLOCK-----
lQPGBGRnZ2EBCADQjsT3n6jj4dJFVFbMaZNe9p4Ohfe3kTPJyiLZJR5Kj9mK8sd7
-----END PGP PRIVATE KEY BLOCK-----`;
      const matches = detectSecrets(privateKey);
      expect(matches.some((m) => m.ruleId === 'private-key')).toBe(true);
    });

    test('rejects minimal content private key format', () => {
      // From gitleaks: private key format with minimal content should not match
      const notPrivateKey = `-----BEGIN PRIVATE KEY-----
anything
-----END PRIVATE KEY-----`;
      const matches = detectSecrets(notPrivateKey);
      expect(matches.some((m) => m.ruleId === 'private-key')).toBe(false);
    });
  });
});
