// `bun run --filter @alignment-hive/plugins typecheck [plugin...]`: type-checks plugins' hooks modules and tests, and
// the retrieval bench, which imports hive's recall code. Bare `tsc` is denied in this project, so this is the way in.
// Claude Code writes its declarations (the module 'claude-code') only beside a mod it loads, or when the
// plugin-authoring skill loads, so this borrows the newest copy on the machine instead of tracking one.
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'

const here = import.meta.dir
const configDir = process.env.CLAUDE_CONFIG_DIR ?? join(homedir(), '.claude')
const DECLARATIONS = [
  [`/tmp/claude-${process.getuid?.()}/bundled-skills`, '*/*/plugin-authoring/types/claude-code.d.ts'],
  [join(configDir, 'dev-mods'), '*/*/.claude-plugin/types/claude-code/index.d.ts'],
  [join(configDir, 'plugins/cache'), '*/*/*/.claude-plugin/types/claude-code/index.d.ts'],
] as const

function newestDeclarations(): string | undefined {
  const found = DECLARATIONS.flatMap(([cwd, pattern]) => {
    try {
      return [...new Bun.Glob(pattern).scanSync({ cwd, absolute: true, dot: true })]
    } catch {
      return [] // That place doesn't exist on this machine.
    }
  })
  return found.map(path => ({ path, at: statSync(path).mtimeMs })).sort((a, b) => b.at - a.at)[0]?.path
}

function pluginsWithModules(): string[] {
  return readdirSync(here).filter(name => {
    try {
      return 'modules' in JSON.parse(readFileSync(join(here, name, 'hooks/hooks.json'), 'utf8'))
    } catch {
      return false
    }
  })
}

const tsc = (config: string) => Bun.spawnSync(['bun', 'x', '--no-install', 'tsc', '-p', config], { stdout: 'inherit', stderr: 'inherit' }).exitCode

const declarations = newestDeclarations()
if (!declarations) {
  console.error('No Claude Code type declarations on this machine. Load the plugin-authoring skill in any session (it writes them), then run this again.')
  process.exit(1)
}
console.log(`${readFileSync(declarations, 'utf8').split('\n', 1)[0]?.replace('// ', '')} (${declarations})`)

const dir = mkdtempSync(join(tmpdir(), 'plugins-typecheck-'))
let failed = 0
for (const plugin of process.argv.length > 2 ? process.argv.slice(2) : pluginsWithModules()) {
  const root = join(here, plugin)
  const config = join(dir, `${plugin}.json`)
  writeFileSync(config, JSON.stringify({
    compilerOptions: {
      // The options the declarations' header gives a hooks module.
      target: 'es2023', lib: ['es2023'], types: [],
      module: 'esnext', moduleResolution: 'bundler',
      strict: true, noUncheckedIndexedAccess: true,
      noEmit: true, skipLibCheck: true,
      jsx: 'react', jsxFactory: 'h', jsxFragmentFactory: 'Fragment',
    },
    include: [declarations, join(root, 'hooks'), join(root, 'types'), join(root, 'tests')],
    exclude: [join(root, 'hooks/*.spec.ts')], // Those run under bun:test, with Node's modules.
  }))
  console.log(`\n${plugin}`)
  if (tsc(config) !== 0) failed++
}
rmSync(dir, { recursive: true })
if (process.argv.length <= 2) {
  console.log('\nexperiments/auto-retrieval/bench')
  if (tsc(join(here, '../experiments/auto-retrieval/bench')) !== 0) failed++
}
process.exit(failed === 0 ? 0 : 1)
