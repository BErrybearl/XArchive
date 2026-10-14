import { mount } from 'svelte';
import App from './App.svelte';
import './app.css';
import { applyTheme, readTheme } from './lib/theme';

// Apply before mounting so a dark OS cannot flash the default light UI dark.
applyTheme(readTheme());

const target = document.getElementById('app');
if (!target) {
  throw new Error('XArchive root element is missing.');
}

mount(App, { target });
