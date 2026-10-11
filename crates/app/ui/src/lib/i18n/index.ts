// Message lookup. Components never contain UI text; they call `t('section.key', params)`.
// Adding a language means adding a catalog of type `Messages` and selecting it here.
import { ja } from './ja';

type Widen<T> = { [K in keyof T]: T[K] extends string ? string : Widen<T[K]> };

/** Shape every catalog must have. */
export type Messages = Widen<typeof ja>;

type Leaves<T, P extends string = ''> = {
  [K in keyof T & string]: T[K] extends string ? `${P}${K}` : Leaves<T[K], `${P}${K}.`>;
}[keyof T & string];

/** Every valid message key, e.g. `'driveSelect.title'`. */
export type MessageKey = Leaves<Messages>;

export type Params = Record<string, string | number>;

/** BCP 47 tag used for number and date formatting. */
export const locale = 'ja-JP';
const messages: Messages = ja;

function lookup(key: MessageKey): string {
  let node: unknown = messages;
  for (const part of key.split('.')) {
    node = (node as Record<string, unknown>)[part];
  }
  return typeof node === 'string' ? node : key;
}

/** A message split at its placeholders, so placeholders can be styled (e.g. monospace numbers). */
export type Part = { text: string; param: string | null };

export function parts(key: MessageKey, params: Params = {}): Part[] {
  return lookup(key)
    .split(/(\{\w+\})/)
    .filter((s) => s !== '')
    .map((s) => {
      const name = /^\{(\w+)\}$/.exec(s)?.[1];
      if (name !== undefined && name in params) {
        return { text: formatParam(params[name]), param: name };
      }
      return { text: s, param: null };
    });
}

export function t(key: MessageKey, params: Params = {}): string {
  return parts(key, params)
    .map((p) => p.text)
    .join('');
}

const numberFormat = new Intl.NumberFormat(locale);

function formatParam(value: string | number | undefined): string {
  return typeof value === 'number' ? numberFormat.format(value) : (value ?? '');
}
