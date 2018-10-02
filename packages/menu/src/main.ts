import { DemoMenu } from './demo-menu';

window.customElements.define('demo-menu', DemoMenu);





const _$demoMenu: any = document.querySelector('demo-menu');




_$demoMenu.data = [
  {
    title: 'Стили',
    hide: false,
    links: [
      {
        id: 'grid',
        title: 'Grid',
        state: 'showin',
      },
      {
        id: 'headers',
        title: 'Headers',
        state: 'showin',
      },
      {
        id: 'input',
        title: 'Input',
        state: 'showin',
      },
      {
        id: 'input-group',
        title: 'Input Group',
        state: 'showin',
      },
      {
        id: 'layout-form',
        title: 'Layout Form',
        state: 'showin',
      },
      {
        id: 'button-group',
        title: 'Button Group',
        state: 'showin',
      },

    ],
  }, {
    title: 'Веб-компоненты',
    hide: true,
    links: [
      {
        id: 'spinner',
        title: 'Spinner',
        state: 'showin',
      },
      {
        id: 'slider',
        title: 'Slider',
        state: 'showin',
      },
    ],
  },
];