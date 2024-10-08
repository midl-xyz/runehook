
       /     /   ▶ Runehook   
      / --- /      Runes indexing service and REST API based on Chainhook
     /     /       

* [Features](#features)
* [Quick Start](#quick-start)
    * [System Requirements](#system-requirements)
    * [Indexing Runes](#indexing-runes)
* [Bugs and Feature Requests](#bugs-and-feature-requests)
* [Contribute](#contribute)
* [Community](#community)

***

# Features

* Runes indexing
    * Etchings, mints, transfers, burns
    * Account balance tracking
    * Rune chainhook predicates (coming soon!)
* REST API endpoints
    * Rune etching and supply information
    * Rune activity per block, transaction, account
    * Balances and holders

# Quick Start

For instructions to run the REST API or endpoint reference, take a look at the [API README](./api/README.md).

## System Requirements

To run runehook, you will need:

1. A fully synchronized Bitcoin node.
1. A local writeable Postgres database for data storage.
   * We recommended a 1TB volume size.

## Getting started

1. Clone the repo and install runehook
    ```
    cargo runehook-install
    ```

1. Generate a new config file. Make sure to fill in the correct information.
    ```
    runehook config new
    ```

1. Start the indexing service
    ```
    runehook service start --config-path Runehook.toml
    ```

# Bugs and feature requests

If you encounter a bug or have a feature request, we encourage you to follow the
steps below:

 1. **Search for existing issues:** Before submitting a new issue, please search
    [existing and closed issues](../../issues) to check if a similar problem or
    feature request has already been reported.
 1. **Open a new issue:** If it hasn't been addressed, please [open a new
    issue](../../issues/new/choose). Choose the appropriate issue template and
    provide as much detail as possible, including steps to reproduce the bug or
    a clear description of the requested feature.
 1. **Evaluation SLA:** Our team reads and evaluates all the issues and pull
    requests. We are avaliable Monday to Friday and we make a best effort to
    respond within 7 business days.

Please **do not** use the issue tracker for personal support requests or to ask
for the status of a transaction. You'll find help at the [#support Discord
channel](https://discord.gg/SK3DxdsP).


# Contribute

Development of this product happens in the open on GitHub, and we are grateful
to the community for contributing bugfixes and improvements. Read below to learn
how you can take part in improving the product.

## Code of Conduct
Please read our [Code of conduct](../../../.github/blob/main/CODE_OF_CONDUCT.md)
since we expect project participants to adhere to it. 

## Contributing Guide
Read our [contributing guide](.github/CONTRIBUTING.md) to learn about our
development process, how to propose bugfixes and improvements, and how to build
and test your changes.

# Community

Join our community and stay connected with the latest updates and discussions:

- [Join our Discord community chat](https://discord.gg/ZQR6cyZC) to engage with
  other users, ask questions, and participate in discussions.

- [Visit hiro.so](https://www.hiro.so/) for updates and subcribing to the
  mailing list.

- Follow [Hiro on Twitter.](https://twitter.com/hirosystems)
