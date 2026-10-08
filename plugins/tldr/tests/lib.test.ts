import { test, expect } from 'claude-code/testing'
import { isLong, lineFor, parseArgs } from '../hooks/lib'

test('isLong needs over 100 words and more than one line', () => {
  const w = Array(101).fill('word').join(' ')
  expect(isLong(w)).toBe(false)
  expect(isLong(w + '\nmore')).toBe(true)
  expect(isLong(Array(99).fill('w').join(' ') + '\nx')).toBe(false)
})

test('lineFor strips a TL;DR prefix and folds newlines', () => {
  expect(lineFor(' TL;DR: done.\nReally. ')).toBe('done. Really.')
})

test('parseArgs', () => {
  expect(parseArgs('')).toBe('toggle')
  expect(parseArgs(' More ')).toBe('more')
  expect(parseArgs('x')).toBe('help')
})
