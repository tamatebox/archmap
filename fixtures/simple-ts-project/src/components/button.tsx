import React from 'react';
import './button.css';
import logo from '@/assets/logo.svg';

export function Button({ label }: { label: string }) {
  return (
    <button>
      <img src={logo} alt="" />
      Don't {label}
    </button>
  );
}

export const Fragment = React.Fragment;
