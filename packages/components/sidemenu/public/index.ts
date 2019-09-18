// import PluginImunifyAvWidget from "../src";

// customElements.define('plugin-imunifyav-widget', PluginImunifyAvWidget);

import { SideMenu } from '../src';

// create MenuComponent
SideMenu;
const demoMenu: SideMenu = document.querySelector('insum-sidemenu');

demoMenu.addEventListener('change-menu-item', e => {
  console.log(e);
  console.log(demoMenu.activeMenuItem);
});

// add data obj to the menu component
demoMenu.data = [
  {
    title: 'Стили',
    hide: false,
    links: [
      {
        id: 'grid',
        title: 'Grid',
      },
      {
        id: 'headers',
        title: 'Headers',
      },
      {
        id: 'input',
        title: 'Input',
      },
      {
        id: 'input-group',
        title: 'Input Group',
      },
      {
        id: 'layout-form',
        title: 'Layout Form',
      },
      {
        id: 'button-group',
        title: 'Button Group',
      },
    ],
  },
  {
    title: 'Веб-компоненты',
    hide: true,
    links: [
      {
        id: 'spinner',
        title: 'Spinner',
      },
      {
        id: 'slider',
        title: 'Slider',
      },
    ],
  },
];
