/**
 * Web components
 */
export class DemoMenu extends HTMLElement {
  private _template: string;
  private _style: string;

  private _data: {
    title: string,
    hide: boolean,
    links: { id: string, title: string, demo: string, state: string }[],
  }[] = [];

  /*** элементы*/
  private _$sidenav: HTMLElement;
  private _$search: HTMLElement;
  private _$categories: HTMLElement;

  constructor() {
    super();
    try {
      // tslint:disable-next-line:no-var-requires
      this._template = require('./demo-menu.html');
      // tslint:disable-next-line:no-var-requires
      this._style = require('./styles/demo-menu.scss');
    } catch (e) {
      this._template = '';
      this._style = '';
    }
  }


  set data(newData) {
    if (this._data !== newData && typeof newData !== 'undefined') {
      this._data = newData;
      this.createList();
    }
  }
  get data() {
    return this._data;
  }


  /**
   * LIFECYCLE
   * Отслеживаемые параметры
   * изменения в данных атрибутах будут непосредственно отслеживаться компонентом
   */
  static get observedAttributes() { return ['data']; }
  attributeChangedCallback(name, oldValue, newValue) {
    if (oldValue !== newValue) {
      switch (name) {
        case 'data':
          this.data = newValue;
          break;
      }
    }
  }


  /**
   * LIFECYCLE
   * Создание компонента
   */
  connectedCallback(): void {
    // tslint:disable-next-line:no-eval
    this.innerHTML = '<style>' + this._style + '</style>' + eval('`' + this._template + '`');


    this._$sidenav = <HTMLElement>document.querySelectorAll('.demo-menu__sidenav')[0];
    this._$search = <HTMLElement>document.querySelectorAll('.demo-menu__sidenav .demo-menu-search input')[0];
    this._$categories = <HTMLElement>document.getElementById('demoMenuCategories');

    this._$search.addEventListener('keyup', (event) => {
      if(event){
        this.search(event.target['value']);
      }
    });

    this.createList();

    this._$search.focus();
  }


  search(searchfor) {
    for (const category of this.data) {
      for (const link of category.links) {
        if (link.title.toLowerCase().indexOf(searchfor.toLowerCase()) >= 0) {
          link.state = 'show';
        } else {
          link.state = 'hide';
        }
      }
    }
    this.updateList();
  }


  createList() {
    this._$categories.innerHTML = '';
    for (const category of this.data) {
      const tempHTMLString = '';
      const categoryElement = document.createElement('div');
      categoryElement.classList.add('demo-menu-category');
      categoryElement.innerHTML = `
       <label class="demo-menu-category__title" for="catid_${category.title}">${category.title}</label>
       <input type="checkbox" id="catid_${category.title}">
    `;

      for (const link of category.links) {
        const linkElement = document.createElement('div');
        linkElement.classList.add('demo-menu-category__link');
        linkElement.innerHTML = link.title;
        categoryElement.appendChild(linkElement);

        linkElement.addEventListener('click', () => {
          document.getElementById('demoContent').innerHTML = link.demo;
        });

        link['html'] = linkElement;
      }

      this._$categories.appendChild(categoryElement);
      category['html'] = categoryElement;
    }
  }


  updateList() {
    for (const category of this.data) {
      let count = 0;
      category['html'].classList.remove('demo-menu-category_hide');
      for (const link of category.links) {
        // Reset Classes
        link['html'].setAttribute('class', 'demo-menu-category__link');

        // Set Current class
        link['html'].classList.add('demo-menu-category__link_' + link.state);
        if (link.state === 'show') {
          count++;
        }
      }
      // Hide if nothing to list!
      if (count === 0) {
        category['html'].classList.add('demo-menu-category_hide');
      }
    }
  }


}
