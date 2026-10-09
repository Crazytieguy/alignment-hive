import type { EngineInterface, RenderElement } from 'claude-code'

// One plugin's rows in the band above the prompt, drawn without `$`: each
// plugin's own `ui.render` hook reads its state and stacks this over what the
// plugins beneath drew, so third-party plugins on the band keep working.
//
// A plugin cannot import another's files, so each plugin that shows notices
// carries this file; plugins/notice-band.test.ts keeps the copies identical.
// The `notices` its check prints are this file's `Notice` as JSON.

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
}

const MARK: Record<Severity, { rank: number; glyph: string; color: string }> = {
  urgent: { rank: 0, glyph: '✖', color: 'error' },
  problem: { rank: 1, glyph: '!', color: 'warning' },
  action: { rank: 2, glyph: '›', color: 'suggestion' },
  info: { rank: 3, glyph: '↑', color: 'ide' },
}

export function noticeRows(
  ui: ReturnType<EngineInterface['ui']['resolve']>,
  plugin: string,
  notices: Notice[],
  { onAction, onDismiss }: NoticeHandlers,
): RenderElement[] {
  const { Box, Button, Text } = ui

  return [...notices]
    .sort((a, b) => MARK[a.severity].rank - MARK[b.severity].rank)
    .map(notice => {
      const mark = MARK[notice.severity]
      const key = `${plugin}:${notice.id}`

      const buttons = (notice.busy ? [] : (notice.actions ?? [])).map((action, index) => (
        <Button
          key={`${key}:${action.id}`}
          label={action.label}
          variant={index === 0 ? 'primary' : 'secondary'}
          onPress={() => onAction(notice, action)}
        />
      ))
      // An info row is a state, not a request: the band's own collapse hides it.
      if (notice.severity !== 'info' && !notice.busy) {
        buttons.push(<Button key={`${key}:dismiss`} label="Dismiss" dimColor onPress={() => onDismiss(notice)} />)
      }

      // The buttons wrap under the text where the row does not fit.
      return (
        <Box key={key} flexDirection="row" flexWrap="wrap" justifyContent="space-between" columnGap={2}>
          <Text>
            <Text color={mark.color} bold>
              {mark.glyph}{' '}
            </Text>
            <Text bold>{plugin}</Text> {notice.busy ? `${notice.text} · ${notice.busy}` : notice.text}
          </Text>
          {/* Grows to fill its line, so the buttons stay right-aligned when they wrap. */}
          <Box flexDirection="row" flexGrow={1} justifyContent="flex-end" columnGap={1}>
            {buttons}
          </Box>
        </Box>
      )
    })
}
