  provider "azuread" {
      use_microsoft_graph = true
  }

  terraform {
  required_version = "~> 1.1.0"
  required_providers {
    azuread = {
      source  = "hashicorp/azuread"
      version = "~> 1.6"
    }
    azurerm = {
      source  = "hashicorp/azurerm"
    }
    helm = {
      source  = "hashicorp/helm"
    }
    kubernetes = {
      source  = "hashicorp/kubernetes"
    }
    local = {
      source  = "hashicorp/local"
    }
    null = {
      source  = "hashicorp/null"
    }
    random = {
      source  = "hashicorp/random"
    }
    template = {
      source  = "hashicorp/template"
    }
    time = {
      source  = "hashicorp/time"
    }
    tls = {
      source  = "hashicorp/tls"
    }
  }
}
