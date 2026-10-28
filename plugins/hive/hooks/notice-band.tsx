// One plugin's rows in the band above the prompt, drawn without `$`: each
// plugin's own `ui.render` hook reads its state and stacks this over what the
// plugins beneath drew, so third-party plugins on the band keep working.
//
// A plugin cannot import another's files, so each plugin that shows notices
// carries this file; plugins/notice-band.test.ts keeps the copies identical.
// The `notices` its check prints are this file's `Notice` as JSON.
import type { EngineInterface, RenderElement, RenderSurface } from 'claude-code'

export type Severity = 'urgent' | 'problem' | 'action' | 'info'

export type NoticeAction = {
  id: string
  label: string
  // command: a slash command; copy: `command` goes on the clipboard, for a
  // terminal command that is interactive; plugin: the plugin's own handler decides.
  kind: 'command' | 'copy' | 'plugin'
  command?: string
}

export type Notice = {
  id: string
  severity: Severity
  // The whole message: rows wrap rather than hide what the person needs to act.
  text: string
  // An info row's state in a few words, for its label in the desktop's footer
  // ("uploads after 24h idle"); without it the label says the whole text.
  short?: string
  actions?: NoticeAction[]
  // Set on "updated" notices: a dismissal holds until the plugin's next
  // version. Without it, a dismissal holds for this session only.
  version?: string
  // What a pressed button is doing ("restarting…"): drawn after the text in
  // place of the buttons, so the row never looks inert. Set by the plugin.
  busy?: string
}

export type NoticeHandlers = {
  onAction: (notice: Notice, action: NoticeAction) => unknown
  onDismiss: (notice: Notice) => unknown
  // Set where info rows can fold into a footer label (the desktop, once the
  // person works): a Hide button folds them.
  onHide?: () => unknown
}

type Ui = ReturnType<EngineInterface['ui']['resolve']>

const MARK: Record<Severity, { rank: number; glyph: string; color: string; icon: string }> = {
  urgent: { rank: 0, glyph: '✖', color: 'error', icon: icon('#e5534b', '<path d="M5.5 5.5l5 5M10.5 5.5l-5 5" stroke="#fff" stroke-width="1.8" stroke-linecap="round"/>') },
  problem: { rank: 1, glyph: '!', color: 'warning', icon: icon('#d29922', '<path d="M8 4.2v4.6" stroke="#fff" stroke-width="1.8" stroke-linecap="round"/><circle cx="8" cy="11.4" r="1.1" fill="#fff"/>') },
  action: { rank: 2, glyph: '›', color: 'suggestion', icon: icon('#8b6cf6', '<path d="M6.5 4.5L10 8l-3.5 3.5" stroke="#fff" stroke-width="1.8" fill="none" stroke-linecap="round" stroke-linejoin="round"/>') },
  info: { rank: 3, glyph: '↑', color: 'ide', icon: icon('#4c8bf5', '<circle cx="8" cy="4.9" r="1.1" fill="#fff"/><path d="M8 7.4v4.3" stroke="#fff" stroke-width="1.8" stroke-linecap="round"/>') },
}

/** A 16px disc in `fill` with a white mark: the severity told by shape as well as colour. */
function icon(fill: string, mark: string): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16"><circle cx="8" cy="8" r="8" fill="${fill}"/>${mark}</svg>`
}

/**
 * Draws `notices` as band rows, most severe first, each keyed and labeled by `plugin`, over
 * `below`, what the plugins beneath drew.
 *
 * The terminal draws a line of text: a glyph, the plugin's name, the message. Every other
 * surface (`e.surface`) is a page, so a row there is an icon, the name dim and the message
 * as a sentence, and an info row's buttons are secondary: a state, not a request.
 */
export function noticeBand(
  ui: Ui,
  surface: RenderSurface,
  plugin: string,
  notices: Notice[],
  { onAction, onDismiss, onHide }: NoticeHandlers,
  below: RenderElement,
): RenderElement {
  const { Box, Button, Text } = ui
  // The terminal's table carries Svg too, so the surface says which this is.
  const Svg = surface !== 'terminal' && 'Svg' in ui ? ui.Svg : null
  const isPage = Svg !== null

  const rows = [...notices]
    .sort((a, b) => MARK[a.severity].rank - MARK[b.severity].rank)
    .map(notice => {
      const mark = MARK[notice.severity]
      const key = `${plugin}:${notice.id}`
      const isInfo = notice.severity === 'info'
      const text = notice.busy ? `${notice.text} · ${notice.busy}` : notice.text

      const buttons = (notice.busy ? [] : (notice.actions ?? [])).map((action, index) => (
        <Button
          key={`${key}:${action.id}`}
          label={action.label}
          variant={index === 0 && !(isPage && isInfo) ? 'primary' : 'secondary'}
          onPress={() => onAction(notice, action)}
        />
      ))
      // An info row is a state, not a request: the band's own collapse hides it on the
      // terminal, its footer label on the desktop.
      if (!isInfo && !notice.busy) {
        // A page draws it as a link, as Hide: the filled buttons are the row's actions.
        buttons.push(<Button key={`${key}:dismiss`} label="Dismiss" plain={isPage || undefined} dimColor onPress={() => onDismiss(notice)} />)
      }
      if (isInfo && onHide) buttons.push(<Button key={`${key}:hide`} label="Hide" plain dimColor onPress={onHide} />)

      const message = Svg ? (
        <Box flexDirection="row" alignItems="center" columnGap={1}>
          <Svg source={mark.icon} alt={notice.severity} width={16} height={16} />
          <Text dimColor>{plugin}</Text>
          <Text>{sentence(text)}</Text>
        </Box>
      ) : (
        <Text>
          <Text color={mark.color} weight="semibold">
            {mark.glyph}{' '}
          </Text>
          <Text weight="semibold">{plugin}</Text> {text}
        </Text>
      )

      // The buttons wrap under the text where the row does not fit; a page keeps them clear of it.
      return (
        <Box
          key={key}
          flexDirection="row"
          flexWrap="wrap"
          alignItems="center"
          justifyContent="space-between"
          columnGap={2}
          rowGap={isPage ? 1 : 0}
        >
          {message}
          {/* Grows to fill its line, so the buttons stay right-aligned when they wrap. */}
          <Box flexDirection="row" flexGrow={1} alignItems="center" justifyContent="flex-end" columnGap={1}>
            {buttons}
          </Box>
        </Box>
      )
    })

  // A page's buttons are taller than its line of text: a gap keeps rows' buttons apart, this
  // plugin's and the ones beneath.
  return (
    <Box flexDirection="column" rowGap={isPage ? 1 : 0}>
      {rows}
      {below}
    </Box>
  )
}

/** The message as a sentence: its first word capitalized, unless it is a name (`gpt-6.1`, `/model`). */
function sentence(text: string): string {
  return /^[a-z]+(\s|$)/.test(text) ? text.charAt(0).toUpperCase() + text.slice(1) : text
}

/**
 * Draws a plugin's info `notices` as one label in the desktop's prompt footer, out of the band
 * while the person works: `plugin: short` for one, a count for more. A press calls `onOpen`,
 * which shows their rows in the band.
 */
export function noticeLabel(ui: Ui, plugin: string, notices: Notice[], onOpen: () => unknown): RenderElement {
  const { Button } = ui
  const [only, ...more] = notices
  const said = only && more.length === 0 ? (only.short ?? only.text) : `${notices.length} notices`
  return <Button key={`${plugin}:label`} label={`${plugin}: ${said}`} plain dimColor onPress={onOpen} />
}
