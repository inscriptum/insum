import { Define, AbstractElement, state } from 'abstract-element';
import litRender from 'abstract-element/render/lit';
import * as litHtml from 'lit-html';
import style from './styles/insum-menu.scss';



/**
 * Interface for input date type
 */
interface IMenuItem {
  title: string;
  hide: boolean;
  links: {
    id: string,
    title: string
  }[];
}



/**
 * Web components
 */
@Define('insum-menu')
export class MenuComponent extends AbstractElement {
  /** implement html type */
  public html = litHtml.html;

  /** list of hided menu items (after filter) */
  private _hideMenuItems: string[] = [];
  /** menu data */
  private _data: IMenuItem[] = [];


  /**
   * Getter and Setter for data obj
   */
  @state()
  set data(newData: IMenuItem[]) {
    if (this._data !== newData && newData !== undefined) {
      this._data = newData;
    }
  }
  get data() { return this._data }


  constructor() {
    super(litRender, false);
  }


  /**
   * LIFECYCLE
   * Создание компонента
   */
  connectedCallback(): void {
    super.connectedCallback();

    (<HTMLElement>this.querySelectorAll('.insum-menu__sidenav .insum-menu-search input')[0]).focus();
  }


  /**
   * RENDER - service function
   */
  render() {
    const demoMenuCategories = this.data.map(menuItem => {
      let itemCountInside = 0;
      const linkElement = menuItem.links.map(link => {
        let cssClass = 'insum-menu-category__link';
        if (this._hideMenuItems.includes(link.id)) {
          cssClass += ' insum-menu-category__link_hide';
        } else {
          itemCountInside++;
        }
        return this.html`<div class="${cssClass}" @click="${(e) => this.handleMenuLinkClick(e, link.title)}">${link.title}</div>`
      }
      );

      const hideClass = itemCountInside === 0 ? ' insum-menu-category_hide' : '';

      return this.html`
      <div class="${'insum-menu-category' + hideClass}">
        <label class="insum-menu-category__title" for="catid_${menuItem.title}">${menuItem.title}</label>
        <input type="checkbox" id="catid_${menuItem.title}"> ${linkElement}
      </div>`
    }
    );

    return this.html`
     <style>
      ${style}
     </style>
      <div class="insum-menu__sidenav">
        <div class="insum-menu-search">
          <i class="material-icons">search</i>
          <input @keyup="${this.handleSearchKeyup.bind(this)}" type="text" placeholder="Поиск" />
        </div>
        ${demoMenuCategories}
      </div>
    `;
  }


  /**
   * Handler for click by menu item
   * @param event - click event
   * @param menuItemTitle - item title
   */
  handleMenuLinkClick(event: MouseEvent, menuItemTitle: string) {
    event.preventDefault();
    document.querySelector('#content').textContent = menuItemTitle;
  }


  /**
   * Handler for keyup inside search field
   * @param event - keyup event
   */
  handleSearchKeyup(event: KeyboardEvent) {
    event.preventDefault();
    if (event && event.target) {
      this.search(event.target['value']);
    }
  }


  /**
   * Filter menu items by text inside search field
   * @param searchfor - text from search field
   */
  search(searchfor: string) {
    this._hideMenuItems.length = 0;

    for (const category of this.data) {
      for (const link of category.links) {
        if (link.title.toLowerCase().indexOf(searchfor.toLowerCase()) === -1) {
          this._hideMenuItems.push(link.id)
        }
      }
    }
    this.forceUpdate();
  }
}
