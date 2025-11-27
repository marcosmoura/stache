import type { ButtonHTMLAttributes, PropsWithChildren, Ref } from 'react';

export type ButtonProps = PropsWithChildren<ButtonHTMLAttributes<HTMLButtonElement>> & {
  active?: boolean;
  ref?: Ref<HTMLButtonElement>;
};
