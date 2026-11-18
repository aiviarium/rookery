FROM ubuntu:26.04
ARG AGENT_USER=agent
ARG OPERATOR_USER=ops
ARG INSTALL_PLAYWRIGHT=true
ARG INSTALL_WEB_TOOLS=true
ARG INSTALL_NATIVE_TOOLS=true
ARG INSTALL_JVM_TOOLS=true
ARG INSTALL_DOTNET_TOOLS=true
ARG INSTALL_SCRIPTING_TOOLS=true
ARG INSTALL_SCIENTIFIC_TOOLS=true
ARG INSTALL_CHROME_TOOLS=true
# JSON list of features of the plugins that come with rookery, such as
# GitHub's CLI; add for example "unity/cli".
ARG PLUGIN_FEATURES='["github/cli"]'
ARG SOFTWARE_RENDERING=true
ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-venv ca-certificates sudo dbus && rm -rf /var/lib/apt/lists/*
COPY provision/requirements.txt /tmp/rookery-requirements.txt
RUN python3 -m venv /opt/ansible && /opt/ansible/bin/pip install -r /tmp/rookery-requirements.txt
# Laid out as the checkout is: the role finds the plugins that come with
# rookery at ../../plugins/rookery.
COPY provision/ansible /opt/rookery/provision/ansible
COPY provision/ansible.cfg /opt/rookery/provision/ansible.cfg
COPY plugins /opt/rookery/plugins
WORKDIR /opt/rookery/provision
RUN /opt/ansible/bin/ansible-playbook -i localhost, -c local ansible/site.yml \
    -e "rookery_user=${AGENT_USER}" -e "rookery_operator_user=${OPERATOR_USER}" \
    -e "{\"rookery_profile\":\"container\",\"rookery_install_playwright\":${INSTALL_PLAYWRIGHT},\"rookery_tools_web\":${INSTALL_WEB_TOOLS},\"rookery_tools_native\":${INSTALL_NATIVE_TOOLS},\"rookery_tools_jvm\":${INSTALL_JVM_TOOLS},\"rookery_tools_dotnet\":${INSTALL_DOTNET_TOOLS},\"rookery_tools_scripting\":${INSTALL_SCRIPTING_TOOLS},\"rookery_tools_scientific\":${INSTALL_SCIENTIFIC_TOOLS},\"rookery_tools_chrome\":${INSTALL_CHROME_TOOLS},\"rookery_plugin_features\":${PLUGIN_FEATURES},\"rookery_software_rendering\":${SOFTWARE_RENDERING}}" \
    && rm -rf /var/lib/apt/lists/*
COPY container/entrypoint.sh /usr/local/bin/rookery-container-session
RUN chmod 755 /usr/local/bin/rookery-container-session
USER ${AGENT_USER}
WORKDIR /srv/agent/workspaces
EXPOSE 6080 3773
ENTRYPOINT ["/usr/local/bin/agent-env", "/usr/local/bin/rookery-container-session"]
