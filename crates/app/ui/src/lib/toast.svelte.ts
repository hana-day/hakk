// Short notices shown at the bottom of the window.
export interface Toast {
  id: number;
  text: string;
  tone: 'info' | 'error';
}

let next = 1;
export const toasts = $state<Toast[]>([]);

export function showToast(text: string, tone: Toast['tone'] = 'info'): void {
  const id = next++;
  toasts.push({ id, text, tone });
  setTimeout(() => {
    const i = toasts.findIndex((t) => t.id === id);
    if (i >= 0) toasts.splice(i, 1);
  }, 5000);
}
