import '@fontsource-variable/noto-sans-jp';
import '@fontsource/jetbrains-mono/500.css';
import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';

const target = document.getElementById('app');
if (target === null) throw new Error('missing #app element');
export default mount(App, { target });
